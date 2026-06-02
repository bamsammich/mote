//! FFI containment for `mote-cef`.
//!
//! # Unsafe & panic discipline (DISCIPLINES.md §1)
//!
//! This module is the ONLY place in the workspace that touches the raw CEF
//! callback boundary, and the only place `unsafe` is used. `unsafe_code` is
//! denied workspace-wide; it is re-enabled here, narrowly, because wrapping a C
//! ABI requires it. Every `unsafe` block documents its safety invariant inline.
//!
//! The release profile is `panic = "abort"`, so an unwind that crosses the C ABI
//! boundary (a Rust panic propagating into CEF's C++ frames) is undefined
//! behaviour. Every callback body that runs Rust logic is therefore wrapped in
//! [`std::panic::catch_unwind`] via [`guard`]: a panic is caught, logged, and
//! converted into the callback's safe default return value, never unwound.
//!
//! The `cef::` types are imported ONLY here and in sibling wrapper modules that
//! this crate owns; nothing they expose escapes `mote-cef`'s public API.
#![allow(
    unsafe_code,
    reason = "FFI with CEF's C ABI requires unsafe; contained to this crate per DISCIPLINES.md §1"
)]
// The `wrap_*!` macros expand to refcount glue we do not author: an `as_base`
// that `transmute`s a reference, and `pub(crate)` wrappers in this private
// module. These lints fire on macro-generated code, not ours, so they are
// allowed module-wide rather than scattered around each macro invocation.
#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs wrap_* macros transmute &base internally; macro-generated, not authored here"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "this is a private module; pub(crate) documents intended crate-internal visibility"
)]

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use cef::rc::Rc as _;
use cef::{
    Browser, BrowserSettings, CefString, Client, DictionaryValue, DisplayHandler, Frame,
    ImplClient, ImplDisplayHandler, ImplFrame, ImplLifeSpanHandler, ImplLoadHandler,
    ImplRenderHandler, ImplRequest, ImplRequestHandler, ImplResourceRequestHandler,
    LifeSpanHandler, LoadHandler, PaintElementType, PopupFeatures, Rect, RenderHandler, Request,
    RequestHandler, ResourceRequestHandler, ReturnValue, ScreenInfo, WindowInfo,
    WindowOpenDisposition, WrapClient, WrapDisplayHandler, WrapLifeSpanHandler, WrapLoadHandler,
    WrapRenderHandler, WrapRequestHandler, WrapResourceRequestHandler, wrap_client,
    wrap_display_handler, wrap_life_span_handler, wrap_load_handler, wrap_render_handler,
    wrap_request_handler, wrap_resource_request_handler,
};

use crate::browser::PageRole;
use crate::interceptor::{RequestDecision, RequestInfo, ResourceInterceptor};
use crate::paint::{PaintFrame, PixelFormat};
use crate::scheme;

/// Runs a callback body, catching any panic so it never unwinds across the C
/// ABI (which would be UB under `panic = "abort"`). On panic, logs to stderr and
/// returns `default`.
///
/// `AssertUnwindSafe` is justified: our callback bodies only touch `Arc`/`Mutex`
/// state we own; a poisoned mutex from a panic is handled by recovering the inner
/// value, so there is no observable broken invariant to leak.
fn guard<T>(default: T, body: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        eprintln!("mote-cef: panic in CEF callback was caught and contained");
        default
    })
}

/// Shared, thread-safe slot holding the latest painted frame plus a paint
/// counter. `on_paint` writes; the owning [`crate::Page`] reads.
///
/// `Arc<Mutex<_>>` (not the spike's `Rc<RefCell<_>>`) because CEF may invoke the
/// render handler from a thread distinct from the reader, and the handle must be
/// `Send`.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameSlot {
    inner: Arc<Mutex<FrameState>>,
}

#[derive(Debug, Default)]
struct FrameState {
    frame: Option<PaintFrame>,
    paint_count: u64,
}

impl FrameSlot {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of `on_paint` deliveries observed so far.
    pub(crate) fn paint_count(&self) -> u64 {
        self.lock().paint_count
    }

    /// The most recent painted frame, if any has been delivered.
    pub(crate) fn latest(&self) -> Option<PaintFrame> {
        self.lock().frame.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FrameState> {
        // Recover from poisoning: a panic in `guard` may poison this, but the
        // protected data is plain pixels with no cross-field invariant to break.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn store(&self, frame: PaintFrame) {
        let mut state = self.lock();
        state.frame = Some(frame);
        state.paint_count += 1;
    }
}

/// The off-screen viewport size reported to CEF via `view_rect`, plus the
/// device scale reported via `screen_info` (CEF's `get_screen_info`).
///
/// The stored width/height are **logical** (DIP) pixels: CEF lays the document
/// out at `view_rect` size and multiplies by `device_scale_factor` to produce
/// the physical paint buffer. The shell works in physical pixels, so
/// [`crate::Page::notify_resized`] divides physical by the scale before storing
/// here. `view_rect` returns the stored logical size verbatim; `screen_info`
/// reports the stored scale.
///
/// Held behind an `Arc` of atomics so the owning [`crate::Page`] can update it
/// on a window resize and the (possibly other-thread) render handler reads the
/// live value each time CEF queries `view_rect`/`screen_info`. The `Page` then
/// calls `host.was_resized()` + `host.notify_screen_info_changed()` to make CEF
/// re-query and re-paint at the new size and scale.
///
/// The scale is stored as the bit pattern of an `f32` ([`f32::to_bits`]) in an
/// `AtomicU32` so it can be read and written atomically without a lock.
#[derive(Debug, Clone)]
pub(crate) struct ViewSize {
    inner: Arc<ViewSizeInner>,
}

#[derive(Debug)]
struct ViewSizeInner {
    /// Logical (DIP) width reported by `view_rect`.
    width: std::sync::atomic::AtomicI32,
    /// Logical (DIP) height reported by `view_rect`.
    height: std::sync::atomic::AtomicI32,
    /// Device scale factor, stored as `f32::to_bits`; default `1.0`.
    device_scale_bits: std::sync::atomic::AtomicU32,
}

impl ViewSize {
    /// A new shared size starting at logical `width`×`height` and device scale
    /// `1.0` (the shell pushes the real scale via
    /// [`crate::Page::notify_resized`] right after page creation).
    pub(crate) fn new(width: i32, height: i32) -> Self {
        Self {
            inner: Arc::new(ViewSizeInner {
                width: std::sync::atomic::AtomicI32::new(width),
                height: std::sync::atomic::AtomicI32::new(height),
                device_scale_bits: std::sync::atomic::AtomicU32::new(1.0_f32.to_bits()),
            }),
        }
    }

    /// The current logical width (read by `view_rect`).
    pub(crate) fn width(&self) -> i32 {
        self.inner.width.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The current logical height (read by `view_rect`).
    pub(crate) fn height(&self) -> i32 {
        self.inner.height.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The current device scale factor (read by `screen_info`).
    pub(crate) fn device_scale(&self) -> f32 {
        f32::from_bits(
            self.inner
                .device_scale_bits
                .load(std::sync::atomic::Ordering::SeqCst),
        )
    }

    /// Store an explicit logical size and device scale. The `Page` derives the
    /// logical size from the physical size and scale in
    /// [`crate::Page::notify_resized`], then calls `was_resized()` +
    /// `notify_screen_info_changed()`.
    pub(crate) fn set(&self, width: i32, height: i32, device_scale: f32) {
        self.inner
            .width
            .store(width, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .height
            .store(height, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .device_scale_bits
            .store(device_scale.to_bits(), std::sync::atomic::Ordering::SeqCst);
    }

    /// Convert a **physical** size to the **logical** (DIP) size CEF should lay
    /// out at: `round(physical / device_scale)`, clamped to a minimum of 1px so
    /// `view_rect` never reports a zero (or negative) dimension. A non-positive
    /// or non-finite scale falls back to `1.0` (physical == logical).
    pub(crate) fn physical_to_logical(physical: u32, device_scale: f64) -> i32 {
        let scale = if device_scale.is_finite() && device_scale > 0.0 {
            device_scale
        } else {
            1.0
        };
        let logical = (f64::from(physical) / scale).round();
        // physical is u32 and scale > 0; `logical` fits comfortably in i32 for
        // any real display, but clamp defensively to the i32 range and >= 1.
        let logical = logical.clamp(1.0, f64::from(i32::MAX));
        // The clamp guarantees `logical` is finite and within [1, i32::MAX], so
        // the cast is in-range and the fractional part is already removed by
        // `round()` — no truncation or sign surprise.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to [1, i32::MAX] and rounded above; in-range and lossless"
        )]
        let logical = logical as i32;
        logical
    }
}

// ---------------------------------------------------------------------------
// RenderHandler — the CPU on_paint path.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct RenderState {
    slot: FrameSlot,
    size: ViewSize,
}

wrap_render_handler! {
    struct RenderHandlerImpl {
        state: RenderState,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            guard((), || {
                if let Some(r) = rect {
                    // Stored dims are already LOGICAL (DIP): the `Page` divides
                    // the physical size by the device scale before storing. CEF
                    // multiplies this by `device_scale_factor` (from
                    // `screen_info`) to produce the physical paint buffer.
                    r.x = 0;
                    r.y = 0;
                    r.width = self.state.size.width();
                    r.height = self.state.size.height();
                }
            });
        }

        /// Report the off-screen display's properties to CEF (CEF's
        /// `get_screen_info`). The `device_scale_factor` is what makes CEF
        /// render the OSR document high-DPI aware: it lays out at the logical
        /// `view_rect` size and scales the paint buffer by this factor, so the
        /// chrome HTML is laid out at the correct DIP size on a high-DPI monitor
        /// instead of treating physical pixels as CSS pixels (issue #7).
        ///
        /// Returns `1` to tell CEF the populated `ScreenInfo` is authoritative.
        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                let Some(info) = screen_info else {
                    return 0;
                };
                let logical_w = self.state.size.width();
                let logical_h = self.state.size.height();
                info.device_scale_factor = self.state.size.device_scale();
                info.depth = 24;
                info.depth_per_component = 8;
                info.is_monochrome = 0;
                let rect = Rect {
                    x: 0,
                    y: 0,
                    width: logical_w,
                    height: logical_h,
                };
                info.rect = rect.clone();
                info.available_rect = rect;
                1
            })
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            guard((), || {
                // Only the main view; ignore popup/widget layers for v0.1.
                if type_ != PaintElementType::default()
                    || buffer.is_null()
                    || width <= 0
                    || height <= 0
                {
                    return;
                }
                // Guarded above: width > 0 && height > 0, so the unsigned casts
                // are lossless.
                let w = width.cast_unsigned();
                let h = height.cast_unsigned();
                // Use checked arithmetic to guard against 32-bit overflow: on a
                // 32-bit target `usize` is 4 bytes; w*h*4 can exceed usize::MAX
                // for large frames. Bail with a log rather than truncating the
                // slice length (which would be unsound with from_raw_parts).
                let n = (w as usize)
                    .checked_mul(h as usize)
                    .and_then(|p| p.checked_mul(4));
                let Some(n) = n else {
                    eprintln!(
                        "mote-cef: on_paint frame too large for usize ({w}×{h}×4); skipping"
                    );
                    return;
                };
                // SAFETY: CEF guarantees `buffer` points to `width * height * 4`
                // valid BGRA bytes for the duration of this callback. We copy
                // immediately into an owned Vec; the pointer is not retained.
                let pixels = unsafe { std::slice::from_raw_parts(buffer, n) }.to_vec();
                self.state.slot.store(PaintFrame {
                    width: w,
                    height: h,
                    format: PixelFormat::Bgra8,
                    pixels,
                });
            });
        }
    }
}

// ---------------------------------------------------------------------------
// LoadHandler — surfaces navigation/loading state to the Page handle.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct LoadState {
    nav: NavState,
}

/// Thread-safe navigation flags updated by the load handler and read by `Page`.
#[derive(Debug, Clone, Default)]
pub(crate) struct NavState {
    inner: Arc<Mutex<NavFlags>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct NavFlags {
    is_loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
}

impl NavState {
    pub(crate) fn snapshot(&self) -> (bool, bool, bool) {
        let f = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (f.is_loading, f.can_go_back, f.can_go_forward)
    }
}

wrap_load_handler! {
    struct LoadHandlerImpl {
        state: LoadState,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            guard((), || {
                let mut f = self
                    .state
                    .nav
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                f.is_loading = is_loading != 0;
                f.can_go_back = can_go_back != 0;
                f.can_go_forward = can_go_forward != 0;
            });
        }
    }
}

// ---------------------------------------------------------------------------
// DisplayHandler — surfaces the document title to the Page handle.
// ---------------------------------------------------------------------------

/// Thread-safe holder for the page's most recent document title, updated by the
/// display handler's `on_title_change` and read by the owning [`crate::Page`].
///
/// CEF may invoke the display handler from a thread distinct from the reader, so
/// the slot is `Arc<Mutex<_>>` (matching [`FrameSlot`]/[`NavState`]) and the
/// handle is `Send`.
#[derive(Debug, Clone, Default)]
pub(crate) struct TitleSlot {
    inner: Arc<Mutex<Option<String>>>,
}

impl TitleSlot {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The most recently observed document title, if CEF has reported one.
    pub(crate) fn get(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set(&self, title: Option<String>) {
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = title;
    }
}

#[derive(Clone)]
struct DisplayState {
    title: TitleSlot,
}

wrap_display_handler! {
    struct DisplayHandlerImpl {
        state: DisplayState,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            guard((), || {
                // An empty title (CEF reports `""` before the document sets one)
                // is stored as `None` so the host can fall back to the URL.
                let t = title.map(ToString::to_string).filter(|s| !s.is_empty());
                self.state.title.set(t);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// LifeSpanHandler — intercepts popup windows (ADR-0011).
// ---------------------------------------------------------------------------

/// A popup URL intercepted by `on_before_popup` (ADR-0011).
///
/// CEF would otherwise open a new chromeless OS window. The shell drains these
/// each tick and routes each one to an in-window tab in the current workspace.
#[derive(Debug, Clone)]
pub struct PopupTabRequest {
    /// The target URL the popup would have navigated to.
    pub url: String,
    /// Whether the popup was triggered by a direct user gesture (a click).
    ///
    /// `true` → open the new tab in the **foreground** (Chrome convention:
    /// click-driven popups take focus). `false` → JS-initiated popup with no
    /// preceding click; open in the **background** to reduce focus-stealing
    /// (ad windows, OAuth redirects, etc.).
    pub user_gesture: bool,
}

/// A thread-safe queue of [`PopupTabRequest`]s written by the CEF lifespan
/// callback and drained each tick by the shell's `about_to_wait` pump.
///
/// `Arc<Mutex<_>>` (not `channel`) because the same pattern is used throughout
/// `mote-cef` (see [`FrameSlot`], [`NavState`], [`TitleSlot`]); a poisoned
/// mutex is recovered the same way.
#[derive(Debug, Clone, Default)]
pub(crate) struct PopupTabQueue {
    inner: Arc<Mutex<VecDeque<PopupTabRequest>>>,
}

impl PopupTabQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Drain all pending requests. The shell calls this each tick; it never
    /// blocks: if the mutex is poisoned the inner value is recovered and
    /// drained normally (no `PopupTabRequest` carries cross-field invariants).
    pub(crate) fn drain(&self) -> Vec<PopupTabRequest> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    fn push(&self, req: PopupTabRequest) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(req);
    }
}

#[derive(Clone)]
struct LifeSpanState {
    popups: PopupTabQueue,
}

wrap_life_span_handler! {
    struct LifeSpanHandlerImpl {
        state: LifeSpanState,
    }

    impl LifeSpanHandler {
        /// CEF calls this before creating a popup browser. Returning `1` instructs
        /// CEF to abandon its popup pipeline entirely; Mote then opens the target URL
        /// as an in-window tab (ADR-0011).
        ///
        /// The out-parameters (`window_info`, `client`, `settings`, `extra_info`,
        /// `no_javascript_access`) are intentionally ignored: we return `true` before
        /// reading or writing any of them, as the CEF docs state that returning `true`
        /// cancels popup creation regardless of their values.
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            guard(1, || {
                let url = target_url
                    .map(ToString::to_string)
                    .unwrap_or_default();
                // Skip blank / empty targets — no useful URL to open.
                if !url.is_empty() {
                    self.state.popups.push(PopupTabRequest {
                        url,
                        user_gesture: user_gesture != 0,
                    });
                }
                // Return 1 = suppress the OS popup window (CEF abandons its
                // popup pipeline; Mote opens the URL as an in-window tab).
                1
            })
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceRequestHandler / RequestHandler — the network interception seam.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct InterceptState {
    interceptor: Arc<dyn ResourceInterceptor>,
    /// The trust role of the owning page. Content pages are barred from
    /// committing any `mote://` top-level navigation (the S1 nav guard).
    role: PageRole,
}

fn request_info(request: Option<&mut Request>, is_nav: i32, is_dl: i32) -> RequestInfo {
    let (url, method) = request.map_or_else(
        || (String::new(), String::new()),
        |r| {
            (
                CefString::from(&r.url()).to_string(),
                CefString::from(&r.method()).to_string(),
            )
        },
    );
    RequestInfo {
        url,
        method,
        is_navigation: is_nav != 0,
        is_download: is_dl != 0,
    }
}

wrap_resource_request_handler! {
    struct ResourceRequestHandlerImpl {
        state: InterceptState,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut cef::Callback>,
        ) -> ReturnValue {
            guard(ReturnValue::CONTINUE, || {
                let info = request_info(request, 0, 0);
                match self.state.interceptor.on_before_request(&info) {
                    RequestDecision::Allow => ReturnValue::CONTINUE,
                    RequestDecision::Block => ReturnValue::CANCEL,
                }
            })
        }
    }
}

/// The pure decision behind the S1 navigation guard: should a top-level browse
/// to `url` be cancelled for a page of `role`?
///
/// A `Content` (untrusted) page is barred from committing ANY top-level `mote://`
/// navigation; `Chrome`/`Overlay` (trusted, shell-created) pages are exempt.
/// Subframe loads (`is_main == false`) are never cancelled. Extracted so the
/// decision is unit-testable without a live CEF frame/request.
fn should_cancel_navigation(role: PageRole, is_main: bool, url: &str) -> bool {
    is_main && !role.may_navigate_mote_scheme() && scheme::is_mote_scheme(url)
}

wrap_request_handler! {
    struct RequestHandlerImpl {
        state: InterceptState,
    }

    impl RequestHandler {
        /// LAYER 0 — the content-page navigation guard (S1, defence-in-depth).
        ///
        /// Cancels any top-level navigation whose target is a `mote://` URL when
        /// the owning page is the untrusted `Content` role. A content page can
        /// thus NEVER commit a `mote://chrome` (or any internal `mote://`)
        /// navigation, regardless of CEF's LOCAL-scheme link policy — so even if
        /// the renderer origin gate were ever defeated, content could not reach a
        /// privileged-origin document. Trusted shell-created surfaces (Chrome,
        /// Overlay) are exempt so they can load their own internal URLs.
        ///
        /// Returns 1 to cancel the navigation, 0 to allow it.
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                // Only gate top-level (main-frame) navigations. Subframe loads
                // can never carry the privileged origin and are out of scope.
                let is_main = frame.is_some_and(|f| f.is_main() != 0);
                let url = request
                    .as_ref()
                    .map(|r| CefString::from(&r.url()).to_string())
                    .unwrap_or_default();
                ::std::os::raw::c_int::from(should_cancel_navigation(self.state.role, is_main, &url))
            })
        }

        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            guard(None, || {
                Some(ResourceRequestHandlerImpl::new(InterceptState {
                    interceptor: Arc::clone(&self.state.interceptor),
                    role: self.state.role,
                }))
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Client — wires the render, load, request, display, and lifespan handlers.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ClientState {
    render: RenderHandler,
    load: LoadHandler,
    request: RequestHandler,
    display: DisplayHandler,
    life_span: LifeSpanHandler,
}

wrap_client! {
    struct ClientImpl {
        state: ClientState,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            guard(None, || Some(self.state.render.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            guard(None, || Some(self.state.load.clone()))
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            guard(None, || Some(self.state.request.clone()))
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            guard(None, || Some(self.state.display.clone()))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            guard(None, || Some(self.state.life_span.clone()))
        }
    }
}

/// Builds a fully-wired CEF [`Client`] for an off-screen browser of trust `role`.
///
/// Returns the client plus the [`FrameSlot`], [`NavState`], [`TitleSlot`],
/// [`ViewSize`], and [`PopupTabQueue`] the owning `Page` reads from. The
/// `PopupTabQueue` is written by [`LifeSpanHandlerImpl::on_before_popup`] (which
/// intercepts CEF popup windows per ADR-0011) and drained each tick by the shell.
///
/// Keeping construction here keeps every `cef::` handler type inside the FFI
/// module. The `role` is wired into the request handler's `on_before_browse`
/// guard: a `Content` page can never commit a top-level `mote://` navigation (S1).
pub(crate) fn build_client(
    size: ViewSize,
    interceptor: Arc<dyn ResourceInterceptor>,
    role: PageRole,
) -> (
    Client,
    FrameSlot,
    NavState,
    TitleSlot,
    ViewSize,
    PopupTabQueue,
) {
    let slot = FrameSlot::new();
    let nav = NavState::default();
    let title = TitleSlot::new();
    let popups = PopupTabQueue::new();

    let render = RenderHandlerImpl::new(RenderState {
        slot: slot.clone(),
        size: size.clone(),
    });
    let load = LoadHandlerImpl::new(LoadState { nav: nav.clone() });
    let request = RequestHandlerImpl::new(InterceptState { interceptor, role });
    let display = DisplayHandlerImpl::new(DisplayState {
        title: title.clone(),
    });
    let life_span = LifeSpanHandlerImpl::new(LifeSpanState {
        popups: popups.clone(),
    });

    let client = ClientImpl::new(ClientState {
        render,
        load,
        request,
        display,
        life_span,
    });

    (client, slot, nav, title, size, popups)
}

// ---------------------------------------------------------------------------
// Helpers used by tests (and extracted for testability)
// ---------------------------------------------------------------------------

/// Compute `w * h * 4` as a `usize` with overflow protection.
///
/// Returns `None` if the result would not fit in `usize` (relevant on 32-bit
/// targets). This is the same arithmetic that `on_paint` uses to compute the
/// pixel-buffer length before calling `from_raw_parts`.
#[allow(dead_code)] // compiled only when tests use it; never dead in test cfg
pub(crate) fn pixel_buf_len(w: u32, h: u32) -> Option<usize> {
    (w as usize)
        .checked_mul(h as usize)
        .and_then(|p| p.checked_mul(4))
}

#[cfg(test)]
mod tests {
    use super::{
        PopupTabQueue, PopupTabRequest, ViewSize, pixel_buf_len, should_cancel_navigation,
    };
    use crate::browser::PageRole;

    #[test]
    fn view_size_hidpi_reports_logical_and_scale() {
        // The HiDPI repro from issue #7: a 1858×2098 physical frame at scale
        // 1.25 must lay out at 1486×1678 DIP, and `screen_info` must report 1.25.
        let size = ViewSize::new(1280, 800);
        let scale = 1.25_f64;
        let lw = ViewSize::physical_to_logical(1858, scale);
        let lh = ViewSize::physical_to_logical(2098, scale);
        size.set(lw, lh, 1.25_f32);

        assert_eq!(size.width(), 1486); // round(1858 / 1.25) = round(1486.4)
        assert_eq!(size.height(), 1678); // round(2098 / 1.25) = round(1678.4)
        assert!((size.device_scale() - 1.25).abs() < f32::EPSILON);
    }

    #[test]
    fn view_size_scale_one_is_identity() {
        // At scale 1.0 the logical size equals the physical size unchanged.
        assert_eq!(ViewSize::physical_to_logical(1858, 1.0), 1858);
        assert_eq!(ViewSize::physical_to_logical(2098, 1.0), 2098);

        let size = ViewSize::new(0, 0);
        size.set(
            ViewSize::physical_to_logical(1858, 1.0),
            ViewSize::physical_to_logical(2098, 1.0),
            1.0,
        );
        assert_eq!(size.width(), 1858);
        assert_eq!(size.height(), 2098);
        assert!((size.device_scale() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn view_size_default_scale_is_one() {
        // A freshly created ViewSize reports scale 1.0 until the shell pushes
        // the real scale via notify_resized.
        let size = ViewSize::new(1280, 800);
        assert!((size.device_scale() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn physical_to_logical_rounds_to_nearest() {
        // 100 / 3 = 33.33… rounds to 33; 200 / 3 = 66.66… rounds to 67.
        assert_eq!(ViewSize::physical_to_logical(100, 3.0), 33);
        assert_eq!(ViewSize::physical_to_logical(200, 3.0), 67);
    }

    #[test]
    fn physical_to_logical_clamps_to_min_one() {
        // A zero physical size never yields a zero (or negative) DIP dimension.
        assert_eq!(ViewSize::physical_to_logical(0, 1.25), 1);
    }

    #[test]
    fn physical_to_logical_bad_scale_falls_back_to_identity() {
        // Non-positive / non-finite scales fall back to 1.0 (physical == logical)
        // rather than dividing by zero or producing a bogus dimension.
        assert_eq!(ViewSize::physical_to_logical(1858, 0.0), 1858);
        assert_eq!(ViewSize::physical_to_logical(1858, -2.0), 1858);
        assert_eq!(ViewSize::physical_to_logical(1858, f64::NAN), 1858);
    }

    #[test]
    fn content_page_cancels_mote_navigation() {
        // A content (untrusted) page must never commit a privileged-origin nav.
        assert!(should_cancel_navigation(
            PageRole::Content,
            true,
            "mote://chrome/index.html"
        ));
        // Any mote:// host, not just chrome (S1 rejects ALL internal navs).
        assert!(should_cancel_navigation(
            PageRole::Content,
            true,
            "mote://overlay/picker.html"
        ));
    }

    #[test]
    fn content_page_allows_web_navigation() {
        // Ordinary web navigation is unaffected.
        assert!(!should_cancel_navigation(
            PageRole::Content,
            true,
            "https://example.com/"
        ));
        assert!(!should_cancel_navigation(
            PageRole::Content,
            true,
            "http://example.com/"
        ));
        assert!(!should_cancel_navigation(
            PageRole::Content,
            true,
            "data:text/html,hi"
        ));
    }

    #[test]
    fn trusted_roles_may_load_mote_urls() {
        // The chrome page and shell overlays legitimately load internal URLs.
        assert!(!should_cancel_navigation(
            PageRole::Chrome,
            true,
            "mote://chrome/index.html"
        ));
        assert!(!should_cancel_navigation(
            PageRole::Overlay,
            true,
            "mote://overlay/picker.html"
        ));
    }

    #[test]
    fn subframe_navigation_is_never_cancelled() {
        // Only top-level navigations are gated; subframes can't carry the origin.
        assert!(!should_cancel_navigation(
            PageRole::Content,
            false,
            "mote://chrome/index.html"
        ));
    }

    #[test]
    fn pixel_buf_len_normal() {
        // 640×400 BGRA: 640 * 400 * 4 = 1 024 000
        assert_eq!(pixel_buf_len(640, 400), Some(1_024_000_usize));
    }

    #[test]
    fn pixel_buf_len_one_by_one() {
        assert_eq!(pixel_buf_len(1, 1), Some(4));
    }

    #[test]
    fn pixel_buf_len_zero_dimension() {
        // Zero dimensions: valid arithmetic, produces 0.
        assert_eq!(pixel_buf_len(0, 400), Some(0));
        assert_eq!(pixel_buf_len(640, 0), Some(0));
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn pixel_buf_len_overflows_on_32bit() {
        // On 32-bit: usize::MAX == 4_294_967_295. 65536 * 65536 * 4 overflows.
        assert_eq!(pixel_buf_len(65536, 65536), None);
    }

    // -----------------------------------------------------------------------
    // PopupTabQueue — the ADR-0011 interception queue.
    // -----------------------------------------------------------------------

    #[test]
    fn popup_queue_starts_empty() {
        // A newly created queue has no pending requests.
        let q = PopupTabQueue::new();
        assert!(q.drain().is_empty(), "fresh queue must be empty");
    }

    #[test]
    fn popup_queue_drain_returns_all_and_clears() {
        // Requests pushed before a drain appear in order; after draining the
        // queue is empty (the LifeSpanHandler won't return stale requests on the
        // next tick).
        let q = PopupTabQueue::new();
        q.push(PopupTabRequest {
            url: "https://example.com/a".to_string(),
            user_gesture: true,
        });
        q.push(PopupTabRequest {
            url: "https://example.com/b".to_string(),
            user_gesture: false,
        });
        let drained = q.drain();
        assert_eq!(drained.len(), 2, "both requests must be returned");
        assert_eq!(drained[0].url, "https://example.com/a");
        assert!(drained[0].user_gesture, "first request is gesture-driven");
        assert_eq!(drained[1].url, "https://example.com/b");
        assert!(!drained[1].user_gesture, "second request is JS-initiated");
        // After draining, the queue must be empty.
        assert!(
            q.drain().is_empty(),
            "queue must be empty after drain — no stale re-delivery"
        );
    }

    #[test]
    fn popup_queue_user_gesture_true_maps_to_foreground() {
        // `user_gesture == true` (click-driven popup) must map to `foreground == true`
        // in the shell's `open_popup_tab` call. This test verifies the data is
        // preserved faithfully through the queue — the foreground decision is made
        // by the shell based on `user_gesture`.
        let q = PopupTabQueue::new();
        q.push(PopupTabRequest {
            url: "https://news.ycombinator.com".to_string(),
            user_gesture: true,
        });
        let req = q.drain().into_iter().next().expect("one request");
        assert!(
            req.user_gesture,
            "gesture=true must round-trip through the queue"
        );
    }

    #[test]
    fn popup_queue_user_gesture_false_maps_to_background() {
        // `user_gesture == false` (JS-initiated popup) must remain false so the
        // shell opens the tab in the background (reduces focus-stealing).
        let q = PopupTabQueue::new();
        q.push(PopupTabRequest {
            url: "https://example.com".to_string(),
            user_gesture: false,
        });
        let req = q.drain().into_iter().next().expect("one request");
        assert!(
            !req.user_gesture,
            "gesture=false must round-trip through the queue"
        );
    }

    #[test]
    fn popup_queue_clone_shares_state() {
        // `PopupTabQueue` is `Clone` (needed so `LifeSpanHandlerImpl` + `Page`
        // can each hold a handle to the same queue). A push through the clone is
        // visible to the original.
        let q = PopupTabQueue::new();
        let q2 = q.clone();
        q2.push(PopupTabRequest {
            url: "https://shared.example.com".to_string(),
            user_gesture: true,
        });
        let drained = q.drain(); // drain from the original
        assert_eq!(
            drained.len(),
            1,
            "push through clone must be visible to original"
        );
        assert_eq!(drained[0].url, "https://shared.example.com");
    }
}
