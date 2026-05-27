//! The privileged internal **`mote://chrome` scheme** (ADR-0005 amendment).
//!
//! Mote's chrome is served from a fixed, privileged internal scheme rather than a
//! runtime `file://`/`data:` URL. This removes the last safe-by-convention seam:
//! the host-bridge no longer gates on a runtime `chrome_url` string agreed across
//! two processes — it gates on the document's **origin**, the compile-time
//! constant [`CHROME_ORIGIN`] (`mote://chrome`).
//!
//! Why a scheme is *stronger* than URL-string matching:
//!
//! - There is no URL to configure in two places, so nothing can diverge.
//! - Web content is `http(s)` and **cannot be served from `mote://`** — only this
//!   crate's registered factory backs the scheme — so origin-based gating is
//!   structurally unforgeable. This is the standard browser approach for internal
//!   pages (`chrome://`, `about:`).
//!
//! # Two registration steps, in the right processes
//!
//! 1. **`OnRegisterCustomSchemes`** must run in *every* process (browser + every
//!    subprocess), declaring `mote` standard/secure/local. This crate wires it
//!    into the bridge [`crate::bridge`] `App` that both
//!    [`crate::bootstrap_with_bridge`] (subprocess) and [`crate::Engine::init`]
//!    (browser) install — see [`register_custom_scheme`].
//! 2. A **scheme handler factory** for the `mote://chrome` origin must be
//!    registered in the **browser process after `cef::initialize`** — see
//!    [`crate::Engine::register_chrome_resources`]. The factory serves bytes from
//!    a host-supplied [`ChromeResources`] map and returns 404 for unknown paths.
//!
//! # The host supplies the assets, not `mote-cef`
//!
//! `mote-cef` MUST NOT depend on `mote-ui`. The host (shell) builds a
//! [`ChromeResources`] mapping `path -> (bytes, content-type)` from its embedded
//! chrome assets and hands it to the engine. `mote-cef` only serves what it is
//! given.
#![allow(
    unsafe_code,
    reason = "SchemeHandlerFactory / ResourceHandler wiring is CEF FFI; contained per DISCIPLINES.md §1"
)]
// The `wrap_*!` macros expand to refcount glue we do not author (an `as_base`
// that transmutes a reference). These lints fire on macro-generated code.
#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs wrap_* macros transmute &base internally; macro-generated, not authored here"
)]

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cef::rc::Rc as _;
use cef::{
    Browser, Callback, CefString, Frame, ImplRequest, ImplResourceHandler, ImplResponse,
    ImplSchemeHandlerFactory, ImplSchemeRegistrar, Request, ResourceHandler, ResourceReadCallback,
    Response, SchemeHandlerFactory, SchemeOptions, SchemeRegistrar, WrapResourceHandler,
    WrapSchemeHandlerFactory, register_scheme_handler_factory, wrap_resource_handler,
    wrap_scheme_handler_factory,
};

/// The privileged internal scheme name. Registered standard/secure/local.
pub const CHROME_SCHEME: &str = "mote";

/// The privileged chrome host (the `mote://chrome` authority).
pub const CHROME_HOST: &str = "chrome";

/// The **unprivileged** overlay host (the `mote://overlay` authority).
///
/// Trusted shell-rendered surfaces that do NOT need the host-bridge (the tab
/// picker, the integrity panel) are served from here instead of `mote://chrome`.
/// They are built and driven by the shell (Rust-side input routing + `eval_js`),
/// so they need no `window.cefQuery`. Crucially this host is a **different
/// origin** than `mote://chrome`, so the renderer origin gate ([`is_chrome_origin`])
/// does NOT match it and the privileged binding is never installed there — even
/// though the document is trusted, it carries no authority it does not use.
pub const OVERLAY_HOST: &str = "overlay";

/// The privileged chrome **origin** — the compile-time constant the host-bridge
/// gates on (ADR-0005 amendment).
///
/// The renderer installs `window.mote` / `window.cefQuery` only for frames whose
/// origin is exactly this; the chrome page is loaded at `mote://chrome/<entry>`.
/// Web content is `http(s)` and can never carry this origin.
pub const CHROME_ORIGIN: &str = "mote://chrome";

/// The unprivileged overlay origin (`mote://overlay`). A distinct origin from
/// [`CHROME_ORIGIN`]; the host-bridge origin gate never matches it.
pub const OVERLAY_ORIGIN: &str = "mote://overlay";

/// Build a `mote://chrome/<path>` URL for the chrome entry document.
#[must_use]
pub fn chrome_url(path: &str) -> String {
    let path = path.strip_prefix('/').unwrap_or(path);
    format!("{CHROME_ORIGIN}/{path}")
}

/// Build a `mote://overlay/<path>` URL for an unprivileged overlay document.
#[must_use]
pub fn overlay_url(path: &str) -> String {
    let path = path.strip_prefix('/').unwrap_or(path);
    format!("{OVERLAY_ORIGIN}/{path}")
}

/// Returns `true` if `url`'s origin is the privileged chrome origin
/// (`mote://chrome`). This is the gate predicate: a frame either *is* the chrome
/// origin or it is not, with no runtime configuration to get wrong. Web content
/// (`http(s)`) can never match, and neither can the unprivileged
/// [`OVERLAY_ORIGIN`].
#[must_use]
pub(crate) fn is_chrome_origin(url: &str) -> bool {
    // Exact origin (rare) or any path under it. We deliberately require the
    // `mote://chrome` authority verbatim — a different host (`mote://evil`,
    // `mote://overlay`) or a different scheme never matches.
    url == CHROME_ORIGIN || url.starts_with(&format!("{CHROME_ORIGIN}/"))
}

/// Returns `true` if `url` is a `mote://` URL of any host. The S1 navigation
/// guard rejects ALL of these for content-role pages: untrusted web content can
/// never commit *any* internal `mote://` navigation, privileged or not.
#[must_use]
pub(crate) fn is_mote_scheme(url: &str) -> bool {
    url.starts_with("mote://")
}

/// Runs a callback body, catching any panic so it never unwinds across the C ABI
/// (UB under `panic = "abort"`). On panic, logs and returns `default`.
fn guard<T>(default: T, body: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        eprintln!("mote-cef: panic in scheme CEF callback was caught and contained");
        default
    })
}

// =============================================================================
// ChromeResources — the host-supplied path -> (bytes, content-type) map.
// =============================================================================

/// One served resource: its bytes, MIME type, and optional charset.
#[derive(Debug, Clone)]
struct Resource {
    bytes: Arc<[u8]>,
    /// The bare MIME type (e.g. `text/html`) — CEF's `set_mime_type` rejects a
    /// full content-type with parameters, so any `; charset=...` is split off.
    mime: String,
    /// The charset parameter (e.g. `utf-8`), if the caller supplied one.
    charset: Option<String>,
}

/// Split a content-type like `text/html; charset=utf-8` into its bare MIME type
/// and optional charset. CEF's `Response::set_mime_type` wants only the MIME
/// type; the charset goes through `set_charset`.
fn split_content_type(content_type: &str) -> (String, Option<String>) {
    let mut parts = content_type.split(';');
    let mime = parts.next().unwrap_or("").trim().to_string();
    let charset = parts.find_map(|p| {
        let p = p.trim();
        p.strip_prefix("charset=")
            .map(|c| c.trim_matches('"').to_string())
    });
    (mime, charset)
}

/// The chrome assets served from `mote://chrome/...`.
///
/// The **host** (shell) builds this from its embedded chrome assets (e.g.
/// `mote-ui`'s) and hands it to [`crate::Engine::register_chrome_resources`].
/// `mote-cef` serves exactly these paths and returns 404 for anything else, so
/// `mote-cef` never depends on `mote-ui`.
///
/// Paths are normalised without a leading slash: registering `index.html` serves
/// both `mote://chrome/index.html` and (as the directory index) `mote://chrome/`.
#[derive(Debug, Clone, Default)]
pub struct ChromeResources {
    /// The entry document path served for a bare `mote://chrome/` request.
    index: Option<String>,
    map: BTreeMap<String, Resource>,
}

impl ChromeResources {
    /// A new, empty resource set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `bytes` to be served at `mote://chrome/<path>` with `content_type`.
    ///
    /// `content_type` accepts either a bare MIME type (`text/html`) or a full
    /// content-type with a charset (`text/html; charset=utf-8`) — the charset is
    /// split off and applied via the response charset, since CEF's `set_mime_type`
    /// wants only the MIME type. The first registered path becomes the directory
    /// index (served for a bare `mote://chrome/`). Returns `self` for chaining.
    #[must_use]
    pub fn register(
        mut self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        content_type: impl Into<String>,
    ) -> Self {
        let path = normalize_path(&path.into());
        if self.index.is_none() {
            self.index = Some(path.clone());
        }
        let (mime, charset) = split_content_type(&content_type.into());
        self.map.insert(
            path,
            Resource {
                bytes: Arc::from(bytes.into().into_boxed_slice()),
                mime,
                charset,
            },
        );
        self
    }

    /// Set the document served for a bare `mote://chrome/` (the directory index).
    /// `path` must already be registered via [`ChromeResources::register`].
    #[must_use]
    pub fn with_index(mut self, path: impl Into<String>) -> Self {
        self.index = Some(normalize_path(&path.into()));
        self
    }

    /// The registered resource paths (diagnostics / tests).
    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    /// Resolve a request `path` (the part after `mote://chrome/`) to a resource.
    fn resolve(&self, path: &str) -> Option<&Resource> {
        let path = normalize_path(path);
        if path.is_empty() {
            return self.index.as_ref().and_then(|i| self.map.get(i));
        }
        self.map.get(&path)
    }
}

/// Strip a leading slash and any query/fragment so map keys are stable.
fn normalize_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.strip_prefix('/').unwrap_or(path).to_string()
}

/// Extract the path portion of a `<origin>/<path>` URL (everything after the
/// origin). Returns `""` for a bare origin.
fn url_path_for(url: &str, origin: &str) -> String {
    let rest = url
        .strip_prefix(&format!("{origin}/"))
        .or_else(|| url.strip_prefix(origin))
        .unwrap_or("");
    normalize_path(rest)
}

// =============================================================================
// Scheme registration (step 1 — runs in ALL processes).
// =============================================================================

/// Declare the `mote` scheme as standard + secure + local on `registrar`. Called
/// from the bridge `App`'s `on_register_custom_schemes`, which CEF invokes in
/// **every** process. Without this in the renderer subprocess, a `mote://` page
/// would not be treated as a secure standard origin.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "SchemeRegistrar borrows a CEF-owned registrar passed as &mut by the App callback; we must not move it out"
)]
pub(crate) fn register_custom_scheme(registrar: &SchemeRegistrar) {
    // STANDARD: parse as a normal hierarchical URL (origin = scheme://host).
    // SECURE:   treated like https (secure context; no mixed-content downgrade).
    // LOCAL:    file-like security (untrusted pages cannot link to / access it).
    // CORS/FETCH: let the chrome document fetch its own same-origin assets.
    let options = SchemeOptions::STANDARD.get_raw()
        | SchemeOptions::SECURE.get_raw()
        | SchemeOptions::CORS_ENABLED.get_raw()
        | SchemeOptions::FETCH_ENABLED.get_raw();
    // `get_raw` returns u32 of small bitflags; the cast is lossless.
    let options = i32::try_from(options).unwrap_or(0);
    registrar.add_custom_scheme(Some(&CefString::from(CHROME_SCHEME)), options);
}

// =============================================================================
// Scheme handler factory + resource handler (step 2 — browser process).
// =============================================================================

/// The state a [`ChromeSchemeFactory`] carries: the resource map plus the origin
/// it serves under (so the same factory type backs both the `mote://chrome` and
/// `mote://overlay` hosts).
#[derive(Clone)]
struct FactoryState {
    resources: Arc<ChromeResources>,
    /// The origin (`mote://chrome` / `mote://overlay`) requests are stripped of
    /// to compute the served path.
    origin: &'static str,
}

wrap_scheme_handler_factory! {
    struct ChromeSchemeFactory {
        state: FactoryState,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            guard(None, || {
                let url = request
                    .as_ref()
                    .map(|r| CefString::from(&r.url()).to_string())
                    .unwrap_or_default();
                let path = url_path_for(&url, self.state.origin);
                Some(self.state.resources.resolve(&path).map_or_else(
                    MemResourceHandler::not_found,
                    |res| {
                        MemResourceHandler::serve(
                            res.bytes.clone(),
                            &res.mime,
                            res.charset.as_deref(),
                        )
                    },
                ))
            })
        }
    }
}

/// In-memory resource handler: serves a fixed byte buffer (or a 404) for one
/// request. Mirrors cef-rs's `StreamResourceHandler` but reads from an owned
/// `Arc<[u8]>` with a cursor rather than a stream.
#[derive(Clone)]
struct ResourceBody {
    bytes: Arc<[u8]>,
    /// Bare MIME type for `Response::set_mime_type` (no `; charset=` parameter).
    mime: String,
    /// Charset for `Response::set_charset`, if known.
    charset: Option<String>,
    status: i32,
    /// Read offset into `bytes`. CEF drives `read` sequentially for one request;
    /// an atomic (vs a `Mutex`) keeps the callback lock-free and Drop-trivial.
    cursor: Arc<AtomicUsize>,
}

wrap_resource_handler! {
    struct MemResourceHandler {
        body: ResourceBody,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                // Handle the request immediately (synchronous, fully in-memory).
                if let Some(h) = handle_request {
                    *h = 1;
                }
                1
            })
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            guard((), || {
                if let Some(response) = response {
                    // A complete status line (code + text) is required, else
                    // Chromium does not treat the response as a renderable
                    // document. set_mime_type wants the BARE MIME type (no
                    // `; charset=` parameter); the charset goes through set_charset.
                    response.set_status(self.body.status);
                    response.set_status_text(Some(&CefString::from(if self.body.status == 200 {
                        "OK"
                    } else {
                        "Not Found"
                    })));
                    response.set_mime_type(Some(&CefString::from(self.body.mime.as_str())));
                    if let Some(charset) = &self.body.charset {
                        response.set_charset(Some(&CefString::from(charset.as_str())));
                    }
                }
                if let Some(len) = response_length {
                    *len = i64::try_from(self.body.bytes.len()).unwrap_or(-1);
                }
            });
        }

        #[allow(
            clippy::not_unsafe_ptr_arg_deref,
            reason = "CEF guarantees data_out points to bytes_to_read writable bytes for this call"
        )]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                let Some(bytes_read) = bytes_read else {
                    return 0;
                };
                if bytes_to_read < 1 || data_out.is_null() {
                    *bytes_read = 0;
                    return 0;
                }
                // Atomically claim the next `n` bytes starting at `start`.
                let total = self.body.bytes.len();
                let want = usize::try_from(bytes_to_read).unwrap_or(0);
                let start = self.body.cursor.load(Ordering::SeqCst);
                if start >= total {
                    *bytes_read = 0;
                    return 0; // EOF — no more data.
                }
                let n = (total - start).min(want);
                self.body.cursor.store(start + n, Ordering::SeqCst);
                // SAFETY: CEF guarantees `data_out` is writable for `bytes_to_read`
                // bytes for the duration of this call, and `n <= bytes_to_read`.
                // The source slice is `n` bytes from our owned buffer at `start`.
                unsafe {
                    std::ptr::copy_nonoverlapping(self.body.bytes.as_ptr().add(start), data_out, n);
                }
                *bytes_read = i32::try_from(n).unwrap_or(0);
                1
            })
        }
    }
}

impl MemResourceHandler {
    /// A 200 handler serving `bytes` with the bare `mime` type and optional
    /// `charset`.
    fn serve(bytes: Arc<[u8]>, mime: &str, charset: Option<&str>) -> ResourceHandler {
        Self::new(ResourceBody {
            bytes,
            mime: mime.to_string(),
            charset: charset.map(str::to_string),
            status: 200,
            cursor: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// A 404 handler with an empty body (unregistered path).
    fn not_found() -> ResourceHandler {
        Self::new(ResourceBody {
            bytes: Arc::from(Vec::new().into_boxed_slice()),
            mime: "text/plain".to_string(),
            charset: Some("utf-8".to_string()),
            status: 404,
            cursor: Arc::new(AtomicUsize::new(0)),
        })
    }
}

/// Register the scheme handler factory for the `mote://chrome` origin, serving
/// `resources`. Must be called in the **browser process after `cef::initialize`**.
/// Crate-internal: reached only via [`crate::Engine::register_chrome_resources`].
pub(crate) fn register_chrome_factory(resources: Arc<ChromeResources>) {
    register_host_factory(CHROME_HOST, CHROME_ORIGIN, resources);
}

/// Register the scheme handler factory for the **unprivileged** `mote://overlay`
/// origin, serving `resources`. Must be called in the **browser process after
/// `cef::initialize`**. Crate-internal: reached only via
/// [`crate::Engine::register_overlay_resources`]. Documents at this origin never
/// receive the host-bridge binding (the origin gate matches only `mote://chrome`).
pub(crate) fn register_overlay_factory(resources: Arc<ChromeResources>) {
    register_host_factory(OVERLAY_HOST, OVERLAY_ORIGIN, resources);
}

/// Register a scheme handler factory for one `mote` host, serving `resources`
/// and stripping `origin` to compute served paths.
fn register_host_factory(host: &str, origin: &'static str, resources: Arc<ChromeResources>) {
    let mut factory: SchemeHandlerFactory =
        ChromeSchemeFactory::new(FactoryState { resources, origin });
    register_scheme_handler_factory(
        Some(&CefString::from(CHROME_SCHEME)),
        Some(&CefString::from(host)),
        Some(&mut factory),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        CHROME_ORIGIN, ChromeResources, OVERLAY_ORIGIN, chrome_url, is_chrome_origin,
        is_mote_scheme, normalize_path, overlay_url, split_content_type, url_path_for,
    };

    #[test]
    fn split_content_type_separates_mime_and_charset() {
        assert_eq!(
            split_content_type("text/html; charset=utf-8"),
            ("text/html".to_string(), Some("utf-8".to_string()))
        );
        assert_eq!(
            split_content_type("text/html"),
            ("text/html".to_string(), None)
        );
        // Tolerate quoting and odd spacing.
        assert_eq!(
            split_content_type("application/json ; charset=\"UTF-8\""),
            ("application/json".to_string(), Some("UTF-8".to_string()))
        );
    }

    #[test]
    fn chrome_origin_gate_matches_only_chrome() {
        assert!(is_chrome_origin("mote://chrome/index.html"));
        assert!(is_chrome_origin("mote://chrome/"));
        assert!(is_chrome_origin(CHROME_ORIGIN));
        // Web content can never match — the whole point.
        assert!(!is_chrome_origin("https://chrome.example.com/"));
        assert!(!is_chrome_origin("http://chrome/"));
        // A different mote host is not the chrome origin.
        assert!(!is_chrome_origin("mote://evil/index.html"));
        assert!(!is_chrome_origin("mote://chromezilla/x"));
        assert!(!is_chrome_origin("data:text/html,hi"));
        // The unprivileged overlay host is NOT the chrome origin — overlays get
        // no `window.cefQuery` binding (S2).
        assert!(!is_chrome_origin("mote://overlay/picker.html"));
        assert!(!is_chrome_origin(OVERLAY_ORIGIN));
    }

    #[test]
    fn mote_scheme_matches_any_internal_host() {
        // The S1 content-guard rejects ALL mote:// hosts for content pages.
        assert!(is_mote_scheme("mote://chrome/index.html"));
        assert!(is_mote_scheme("mote://overlay/picker.html"));
        assert!(is_mote_scheme("mote://evil/x"));
        assert!(is_mote_scheme(CHROME_ORIGIN));
        // Web content and data URLs are never the mote scheme.
        assert!(!is_mote_scheme("https://example.com/"));
        assert!(!is_mote_scheme("http://mote/"));
        assert!(!is_mote_scheme("data:text/html,hi"));
    }

    #[test]
    fn chrome_url_builds_under_origin() {
        assert_eq!(chrome_url("index.html"), "mote://chrome/index.html");
        assert_eq!(chrome_url("/index.html"), "mote://chrome/index.html");
    }

    #[test]
    fn overlay_url_builds_under_overlay_origin() {
        assert_eq!(overlay_url("picker.html"), "mote://overlay/picker.html");
        assert_eq!(overlay_url("/picker.html"), "mote://overlay/picker.html");
    }

    #[test]
    fn url_path_extracts_after_origin() {
        assert_eq!(
            url_path_for("mote://chrome/app.js", CHROME_ORIGIN),
            "app.js"
        );
        assert_eq!(url_path_for("mote://chrome/", CHROME_ORIGIN), "");
        assert_eq!(url_path_for("mote://chrome", CHROME_ORIGIN), "");
        assert_eq!(
            url_path_for("mote://chrome/a/b.css?v=1", CHROME_ORIGIN),
            "a/b.css"
        );
        // Overlay origin is stripped the same way.
        assert_eq!(
            url_path_for("mote://overlay/picker.html", OVERLAY_ORIGIN),
            "picker.html"
        );
    }

    #[test]
    fn normalize_strips_slash_query_fragment() {
        assert_eq!(normalize_path("/index.html"), "index.html");
        assert_eq!(normalize_path("a.css?v=1"), "a.css");
        assert_eq!(normalize_path("a.js#frag"), "a.js");
    }

    #[test]
    fn resolve_serves_index_for_bare_origin() {
        let res = ChromeResources::new()
            .register("index.html", b"<html>".to_vec(), "text/html")
            .register("app.js", b"x=1".to_vec(), "text/javascript");
        // bare origin -> index (first registered)
        assert_eq!(res.resolve("").map(|r| &*r.bytes), Some(&b"<html>"[..]));
        assert_eq!(res.resolve("app.js").map(|r| &*r.bytes), Some(&b"x=1"[..]));
        // unknown path -> None (handler turns this into a 404)
        assert!(res.resolve("missing.png").is_none());
    }

    #[test]
    fn resources_report_registered_paths() {
        let res = ChromeResources::new()
            .register("index.html", b"".to_vec(), "text/html")
            .register("a.css", b"".to_vec(), "text/css");
        assert_eq!(res.paths(), vec!["a.css", "index.html"]);
    }
}
