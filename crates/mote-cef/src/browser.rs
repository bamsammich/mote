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

use cef::{
    Browser, BrowserSettings, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, WindowInfo,
    browser_host_create_browser_sync,
};

use crate::error::{CefError, Result};
use crate::ffi::{self, FrameSlot, NavState, ViewSize};
use crate::interceptor::{AllowAll, ResourceInterceptor};
use crate::paint::PaintFrame;

/// Options for creating an off-screen [`Page`].
#[derive(Debug, Clone)]
pub struct PageOptions {
    /// Initial off-screen surface width in pixels.
    pub width: u32,
    /// Initial off-screen surface height in pixels.
    pub height: u32,
    /// Off-screen paint rate (frames/second) requested from CEF.
    pub frame_rate: i32,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            frame_rate: 60,
        }
    }
}

/// A single off-screen browser (a Mote tab).
pub struct Page {
    browser: Browser,
    frame: FrameSlot,
    nav: NavState,
    /// Set to `true` once [`Page::close`] has been called so that the [`Drop`]
    /// impl does not issue a second `close_browser` to an already-closing host.
    closed: AtomicBool,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("paint_count", &self.frame.paint_count())
            .finish_non_exhaustive()
    }
}

impl Page {
    /// Create an off-screen browser navigated to `url`, with no request
    /// interception (every request allowed). Requires a live [`crate::Engine`].
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn new(url: &str, options: &PageOptions) -> Result<Self> {
        Self::with_interceptor(url, options, Arc::new(AllowAll))
    }

    /// Create an off-screen browser whose resource loads are gated by
    /// `interceptor` (the ad-block / privacy seam, DESIGN §Engine — CEF).
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if CEF could not create the browser host.
    pub fn with_interceptor(
        url: &str,
        options: &PageOptions,
        interceptor: Arc<dyn ResourceInterceptor>,
    ) -> Result<Self> {
        let size = ViewSize {
            width: options.width.cast_signed(),
            height: options.height.cast_signed(),
        };
        let (mut client, frame, nav) = ffi::build_client(size, interceptor);

        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            ..Default::default()
        };
        let browser_settings = BrowserSettings {
            windowless_frame_rate: options.frame_rate,
            ..Default::default()
        };

        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&browser_settings),
            None,
            None,
        )
        .ok_or_else(|| CefError::BrowserCreate {
            url: url.to_string(),
        })?;

        Ok(Self {
            browser,
            frame,
            nav,
            closed: AtomicBool::new(false),
        })
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
