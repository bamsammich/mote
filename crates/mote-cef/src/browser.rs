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

    /// Request that CEF close this browser. After calling, pump the engine a few
    /// times so CEF can tear down the host before [`crate::Engine::shutdown`].
    pub fn close(&self) {
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }
}
