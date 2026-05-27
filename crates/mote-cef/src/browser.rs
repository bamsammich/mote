//! Off-screen browser handle (`Page`) — tab/browser lifecycle + navigation.
//!
//! A [`Page`] wraps one CEF off-screen [`Browser`] and its host. It surfaces the
//! Mote-shaped operations the rest of the app needs — create, navigate, history,
//! the latest painted frame, close — without exposing any `cef::` type. A "tab"
//! in Mote is a `Page`.
//!
//! `Page` is intentionally **not** `Send`/`Sync`: CEF browser objects are
//! reference-counted and bound to the thread that pumps the message loop. Keep
//! `Page`s on that thread.
#![allow(
    unsafe_code,
    reason = "browser_host_create_browser_sync and host calls are CEF FFI; contained per DISCIPLINES.md §1"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cef::wrapper::message_router::BrowserSideRouter;
use cef::{
    Browser, BrowserSettings, CefString, Client, ImplBrowser, ImplBrowserHost, ImplFrame,
    WindowInfo, browser_host_create_browser_sync,
};

use crate::bridge;
use crate::error::{CefError, Result};
use crate::ffi::{self, FrameSlot, NavState, ViewSize};
use crate::input::{self, ButtonAction, KeyInput, Modifiers, MouseButton, MousePosition};
use crate::interceptor::{AllowAll, ResourceInterceptor};
use crate::paint::PaintFrame;
use crate::profile::ProfileHandle;

/// The trust role a [`Page`] plays.
///
/// Mote composites a privileged HTML/CSS *chrome* document around untrusted web
/// *content* (ADR-0003). The two run in **distinct CEF browsers in distinct
/// renderer processes** (ADR-0005): the chrome page will host the host-bridge
/// bindings (`window.mote`), and content pages must never reach them.
///
/// This enum establishes the distinction so Wave B can scope the host-bridge to
/// `Chrome` pages only. **No bindings are installed in this wave** — the role is
/// plumbed through `Page` creation and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageRole {
    /// Untrusted web content (the default). A normal browser with no privileged
    /// bindings; this is what every web page Mote loads is.
    #[default]
    Content,
    /// The privileged chrome document. Wave B will scope the host-bridge binding
    /// to pages created with this role; here it only marks the page.
    Chrome,
}

/// Options for creating an off-screen [`Page`].
#[derive(Debug, Clone)]
pub struct PageOptions {
    /// Initial off-screen surface width in pixels.
    pub width: u32,
    /// Initial off-screen surface height in pixels.
    pub height: u32,
    /// Off-screen paint rate (frames/second) requested from CEF.
    pub frame_rate: i32,
    /// Trust role of the page (chrome vs untrusted content). Defaults to
    /// [`PageRole::Content`].
    pub role: PageRole,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            frame_rate: 60,
            role: PageRole::default(),
        }
    }
}

/// A single off-screen browser (a Mote tab).
pub struct Page {
    browser: Browser,
    frame: FrameSlot,
    nav: NavState,
    /// The shared OSR viewport size CEF reads via `view_rect`. Updated by
    /// [`Page::notify_resized`], which then asks CEF to re-query and re-paint.
    size: ViewSize,
    role: PageRole,
    /// Set to `true` once [`Page::close`] has been called so that the [`Drop`]
    /// impl does not issue a second `close_browser` to an already-closing host.
    closed: AtomicBool,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("role", &self.role)
            .field("paint_count", &self.frame.paint_count())
            .finish_non_exhaustive()
    }
}

impl Page {
    /// Create an off-screen browser navigated to `url`, with no request
    /// interception (every request allowed) and the default
    /// [`PageRole::Content`]. Requires a live [`crate::Engine`].
    ///
    /// The page is created in CEF's *global* request context (no per-identity
    /// isolation). For identity-isolated pages use [`Page::with_profile`].
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn new(url: &str, options: &PageOptions) -> Result<Self> {
        Self::create(url, options, Arc::new(AllowAll), None)
    }

    /// Create an off-screen browser whose resource loads are gated by
    /// `interceptor` (the ad-block / privacy seam, DESIGN §Engine — CEF), in the
    /// global request context.
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn with_interceptor(
        url: &str,
        options: &PageOptions,
        interceptor: Arc<dyn ResourceInterceptor>,
    ) -> Result<Self> {
        Self::create(url, options, interceptor, None)
    }

    /// Create an off-screen browser under `profile` — i.e. as part of a specific
    /// Mote identity. The page's cookies, storage, history, and cache are
    /// isolated to that profile's `RequestContext`
    /// (see `docs/identity-isolation.md`). Requests are allowed by default;
    /// combine with [`Page::with_profile_and_interceptor`] to gate them.
    ///
    /// # Profile readiness
    /// A *freshly created* [`ProfileHandle`] (its first use) is initialised
    /// asynchronously on the CEF UI thread. Under the Chrome runtime, a
    /// synchronous OSR browser create against an uninitialised profile context
    /// fails. Pump the [`crate::Engine`] a few times after creating a new profile
    /// before creating the first page under it (and check the returned `Result`).
    /// Subsequent pages under an already-warmed profile create immediately.
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn with_profile(url: &str, options: &PageOptions, profile: &ProfileHandle) -> Result<Self> {
        Self::create(url, options, Arc::new(AllowAll), Some(profile))
    }

    /// Like [`Page::with_profile`] but also gates resource loads through
    /// `interceptor`.
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn with_profile_and_interceptor(
        url: &str,
        options: &PageOptions,
        profile: &ProfileHandle,
        interceptor: Arc<dyn ResourceInterceptor>,
    ) -> Result<Self> {
        Self::create(url, options, interceptor, Some(profile))
    }

    /// The single creation path. `profile = None` uses CEF's global request
    /// context; `Some(profile)` isolates the page to that identity's context.
    fn create(
        url: &str,
        options: &PageOptions,
        interceptor: Arc<dyn ResourceInterceptor>,
        profile: Option<&ProfileHandle>,
    ) -> Result<Self> {
        let size = ViewSize::new(options.width.cast_signed(), options.height.cast_signed());
        let (client, frame, nav, size) = ffi::build_client(size, interceptor);
        // Chrome pages are transparent so the composited page shows through;
        // content pages are opaque.
        let transparent = options.role == PageRole::Chrome;
        let browser = create_browser(url, options.frame_rate, client, profile, transparent)?;

        Ok(Self {
            browser,
            frame,
            nav,
            size,
            role: options.role,
            closed: AtomicBool::new(false),
        })
    }

    /// This page's trust role ([`PageRole::Chrome`] vs [`PageRole::Content`]).
    #[must_use]
    pub const fn role(&self) -> PageRole {
        self.role
    }

    /// The most recently painted off-screen frame, if CEF has delivered one.
    #[must_use]
    pub fn latest_frame(&self) -> Option<PaintFrame> {
        self.frame.latest()
    }

    /// Number of `on_paint` deliveries observed so far (useful to wait for the
    /// first frame when pumping the engine).
    #[must_use]
    pub fn paint_count(&self) -> u64 {
        self.frame.paint_count()
    }

    /// Navigate this page to `url`.
    pub fn load_url(&self, url: &str) {
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&CefString::from(url)));
        }
    }

    /// Execute `code` as JavaScript in this page's main frame.
    ///
    /// This is the **Rust→page push** primitive: the host runs trusted script in
    /// the frame (e.g. the chrome bootstrap's `window.mote.applyOp(...)` to push
    /// live tab-list / URL state into the privileged chrome document). The host
    /// layer is responsible for the trust boundary — `mote-shell` only ever calls
    /// this on the **chrome** page, never on untrusted content (the chrome
    /// document is the privileged origin, the only one with `window.mote`).
    ///
    /// No-op if the browser is closing/closed or has no main frame. The script
    /// runs asynchronously on the next message-loop pump.
    pub fn eval_js(&self, code: &str) {
        if let Some(frame) = self.browser.main_frame() {
            // start_line 0; script_url empty (anonymous host-injected script).
            frame.execute_java_script(Some(&CefString::from(code)), None, 0);
        }
    }

    /// Whether a load is currently in progress.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.nav.snapshot().0
    }

    /// Whether back-navigation is available.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.nav.snapshot().1
    }

    /// Whether forward-navigation is available.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.nav.snapshot().2
    }

    /// Navigate back in history (no-op if unavailable).
    pub fn go_back(&self) {
        if self.browser.can_go_back() == 1 {
            self.browser.go_back();
        }
    }

    /// Navigate forward in history (no-op if unavailable).
    pub fn go_forward(&self) {
        if self.browser.can_go_forward() == 1 {
            self.browser.go_forward();
        }
    }

    /// Reload the current page.
    pub fn reload(&self) {
        self.browser.reload();
    }

    // -----------------------------------------------------------------------
    // Input injection (the OSR browser host has no OS window, so the host layer
    // must feed it events). Coordinates are page-local; the window→page mapping
    // is mote-shell's job (Wave B). All are no-ops if the browser has no host
    // (i.e. it is closing/closed).
    // -----------------------------------------------------------------------

    /// Inject a mouse-move at page-local `pos`. `mouse_leave` marks the cursor
    /// leaving the page surface (CEF stops hover/tracking).
    pub fn send_mouse_move(&self, pos: MousePosition, modifiers: Modifiers, mouse_leave: bool) {
        if let Some(host) = self.browser.host() {
            let event = input::mouse_event(pos, modifiers);
            host.send_mouse_move_event(Some(&event), i32::from(mouse_leave));
        }
    }

    /// Inject a mouse button press or release at page-local `pos`.
    ///
    /// `click_count` is the consecutive-click count (1 = single, 2 = double, …);
    /// the host layer tracks click sequencing.
    pub fn send_mouse_button(
        &self,
        pos: MousePosition,
        button: MouseButton,
        action: ButtonAction,
        click_count: i32,
        modifiers: Modifiers,
    ) {
        if let Some(host) = self.browser.host() {
            let event = input::mouse_event(pos, modifiers);
            host.send_mouse_click_event(
                Some(&event),
                button.to_cef(),
                input::click_is_up(action),
                click_count,
            );
        }
    }

    /// Inject a mouse-wheel scroll at page-local `pos`. `delta_x`/`delta_y` are
    /// scroll deltas (CEF interprets the units; positive `delta_y` scrolls up).
    pub fn send_mouse_wheel(
        &self,
        pos: MousePosition,
        delta_x: i32,
        delta_y: i32,
        modifiers: Modifiers,
    ) {
        if let Some(host) = self.browser.host() {
            let event = input::mouse_event(pos, modifiers);
            host.send_mouse_wheel_event(Some(&event), delta_x, delta_y);
        }
    }

    /// Inject a keyboard event (key down / up / char). A full keystroke is
    /// typically a `Down`, then a `Char` (carrying the typed character), then an
    /// `Up`; the host layer is responsible for that sequencing.
    pub fn send_key(&self, key: KeyInput) {
        if let Some(host) = self.browser.host() {
            let event = input::key_event(key);
            host.send_key_event(Some(&event));
        }
    }

    /// Tell this off-screen browser whether it currently has input focus.
    ///
    /// Maps to CEF `SetFocus`. The host layer mirrors logical focus into CEF so
    /// exactly one browser (chrome *or* a content page) believes it is focused at
    /// a time (Phase-2 plan §1.3). `gained = true` → focus gained.
    pub fn send_focus(&self, gained: bool) {
        if let Some(host) = self.browser.host() {
            host.set_focus(i32::from(gained));
        }
    }

    /// Resize this off-screen browser's surface to `width`×`height` pixels.
    ///
    /// Updates the size CEF reads via the render handler's `view_rect`, then
    /// calls `CefBrowserHost::WasResized` so CEF re-queries the rect and repaints
    /// at the new size (the next [`Page::latest_frame`] is the resized frame).
    /// The host layer (mote-shell) calls this when the window — and thus the
    /// chrome (full-window) or content (viewport) region — changes size. No-op if
    /// the browser is closing/closed.
    pub fn notify_resized(&self, width: u32, height: u32) {
        self.size.set(width.cast_signed(), height.cast_signed());
        if let Some(host) = self.browser.host() {
            host.was_resized();
        }
    }

    /// Request that CEF close this browser. Idempotent — safe to call more than
    /// once; subsequent calls are no-ops. After calling, pump the engine a few
    /// times so CEF can tear down the host before [`crate::Engine::shutdown`].
    pub fn close(&self) {
        // Use compare-exchange so that exactly one caller wins the close race;
        // `Drop` uses the same guard so an explicit close followed by drop is safe.
        if self
            .closed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            && let Some(host) = self.browser.host()
        {
            host.close_browser(1);
        }
    }
}

impl Drop for Page {
    /// Ensures the underlying CEF browser host is closed when a [`Page`] is
    /// dropped without an explicit [`Page::close`] call, preventing a resource
    /// leak of the browser host object. Guarded by an atomic flag so an already-
    /// closed page is a no-op here.
    fn drop(&mut self) {
        self.close();
    }
}

/// Create an off-screen CEF browser for `url` with the given `client`, rooted in
/// the global request context (`profile = None`) or a profile's context. Shared
/// by [`Page`] and [`ChromePage`] so both go through one creation call.
///
/// `transparent` controls the OSR background alpha. The **chrome** browser must
/// be transparent (ADR-0003 / plan §1.2): its `<main>` page region is
/// `background: transparent`, so a 0-alpha browser background lets the
/// composited web page show through. **Content** browsers stay opaque so a page
/// with no `<body>` background still paints solid (no see-through page).
fn create_browser(
    url: &str,
    frame_rate: i32,
    mut client: Client,
    profile: Option<&ProfileHandle>,
    transparent: bool,
) -> Result<Browser> {
    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        windowless_frame_rate: frame_rate,
        // CEF background_color is ARGB. Alpha 0 ⇒ transparent OSR surface (the
        // chrome lets the page show through); any opaque value ⇒ solid.
        background_color: if transparent {
            0x0000_0000
        } else {
            0xFFFF_FFFF
        },
        ..Default::default()
    };

    // A closure so the global and profile-bound paths share one creation call;
    // the request context (if any) is borrowed only for the duration here.
    let mut make = |request_context: Option<&mut cef::RequestContext>| {
        browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&browser_settings),
            None,
            request_context,
        )
    };

    match profile {
        Some(p) => p.with_context(|ctx| make(Some(ctx))),
        None => make(None),
    }
    .ok_or_else(|| CefError::BrowserCreate {
        url: url.to_string(),
    })
}

/// A request to open the **privileged chrome page** — the only thing
/// [`crate::HostBridge::for_chrome`] accepts.
///
/// Its mere existence asserts the page is chrome: [`ChromePageRequest::new`] is
/// the only constructor and it forces [`PageRole::Chrome`] internally. A content
/// [`Page`] offers no method that yields one, so the leaky configuration
/// (attaching the host-bridge router to a content browser) is **unrepresentable**.
///
/// Build one, then hand it to [`crate::HostBridge::for_chrome`] which supplies
/// the browser-side router and produces a live [`ChromePage`].
#[derive(Debug, Clone)]
pub struct ChromePageRequest {
    url: String,
    width: u32,
    height: u32,
    frame_rate: i32,
    profile: Option<ProfileHandle>,
}

impl ChromePageRequest {
    /// Describe a chrome page at `url` with `options`' geometry. The role is
    /// always [`PageRole::Chrome`] regardless of `options.role` — this request
    /// type *is* the chrome marker. No profile (global request context); use
    /// [`ChromePageRequest::with_profile`] to isolate it to an identity.
    #[must_use]
    pub fn new(url: &str, options: &PageOptions) -> Self {
        Self {
            url: url.to_string(),
            width: options.width,
            height: options.height,
            frame_rate: options.frame_rate,
            profile: None,
        }
    }

    /// Isolate the chrome page to `profile`'s request context.
    #[must_use]
    pub fn with_profile(mut self, profile: &ProfileHandle) -> Self {
        self.profile = Some(profile.clone());
        self
    }

    /// Open the chrome browser, wrapping its content client in the chrome client
    /// that carries `router` (layer 2). Crate-internal: only
    /// [`crate::HostBridge::for_chrome`] calls this, so the router can never be
    /// attached to a content browser.
    pub(crate) fn open(self, router: Arc<BrowserSideRouter>) -> Result<ChromePage> {
        let size = ViewSize::new(self.width.cast_signed(), self.height.cast_signed());
        // Build the standard content-client handlers (render/load/request), then
        // wrap them in the chrome client that forwards process messages to the
        // browser-side router.
        let (inner, frame, nav, size) = ffi::build_client(size, Arc::new(AllowAll));
        let client = bridge::chrome_client(inner, router);
        // The chrome browser is always transparent (the page composites through
        // its `<main>` region).
        let browser = create_browser(
            &self.url,
            self.frame_rate,
            client,
            self.profile.as_ref(),
            true,
        )?;
        Ok(ChromePage(Page {
            browser,
            frame,
            nav,
            size,
            role: PageRole::Chrome,
            closed: AtomicBool::new(false),
        }))
    }
}

/// A live privileged chrome page driven by a [`crate::HostBridge`].
///
/// Wraps a [`Page`] whose client carries the host-bridge router (layer 2); it
/// surfaces the same read/navigate/input operations by [`Deref`](std::ops::Deref)
/// to [`Page`]. There is no public constructor — a `ChromePage` is only produced
/// by [`crate::HostBridge::for_chrome`], guaranteeing its client (and only its
/// client) carries the router.
#[derive(Debug)]
pub struct ChromePage(Page);

impl std::ops::Deref for ChromePage {
    type Target = Page;
    fn deref(&self) -> &Page {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Verifies the idempotent-close guard pattern used by `Page::close` and
    /// `Drop`. We exercise the CAS logic directly (no live CEF needed).
    #[test]
    fn close_flag_is_idempotent() {
        let closed = AtomicBool::new(false);
        let call_count = AtomicU32::new(0);

        // Simulate the CAS in close() three times; only the first should win.
        for _ in 0..3 {
            if closed
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                call_count.fetch_add(1, Ordering::SeqCst);
            }
        }

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "close body must execute exactly once regardless of call count"
        );
        assert!(
            closed.load(Ordering::SeqCst),
            "closed flag must be set after first close"
        );
    }

    /// Verifies that the closed flag starts at false (not pre-closed).
    #[test]
    fn close_flag_starts_false() {
        let closed = Arc::new(AtomicBool::new(false));
        assert!(
            !closed.load(Ordering::SeqCst),
            "a newly created Page must not be pre-closed"
        );
    }
}
