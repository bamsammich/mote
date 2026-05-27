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

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use cef::rc::Rc as _;
use cef::{
    Browser, CefString, Client, DisplayHandler, ImplClient, ImplDisplayHandler, ImplLoadHandler,
    ImplRenderHandler, ImplRequest, ImplRequestHandler, ImplResourceRequestHandler, LoadHandler,
    PaintElementType, Rect, RenderHandler, Request, RequestHandler, ResourceRequestHandler,
    ReturnValue, WrapClient, WrapDisplayHandler, WrapLoadHandler, WrapRenderHandler,
    WrapRequestHandler, WrapResourceRequestHandler, wrap_client, wrap_display_handler,
    wrap_load_handler, wrap_render_handler, wrap_request_handler, wrap_resource_request_handler,
};

use crate::interceptor::{RequestDecision, RequestInfo, ResourceInterceptor};
use crate::paint::{PaintFrame, PixelFormat};

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

/// The off-screen viewport size reported to CEF via `view_rect`.
///
/// Held behind an `Arc` of two atomics so the owning [`crate::Page`] can update
/// it on a window resize and the (possibly other-thread) render handler reads
/// the live value each time CEF queries `view_rect`. The `Page` then calls
/// `host.was_resized()` to make CEF re-query and re-paint at the new size.
#[derive(Debug, Clone)]
pub(crate) struct ViewSize {
    inner: Arc<ViewSizeInner>,
}

#[derive(Debug)]
struct ViewSizeInner {
    width: std::sync::atomic::AtomicI32,
    height: std::sync::atomic::AtomicI32,
}

impl ViewSize {
    /// A new shared size starting at `width`×`height`.
    pub(crate) fn new(width: i32, height: i32) -> Self {
        Self {
            inner: Arc::new(ViewSizeInner {
                width: std::sync::atomic::AtomicI32::new(width),
                height: std::sync::atomic::AtomicI32::new(height),
            }),
        }
    }

    /// The current width (read by `view_rect`).
    pub(crate) fn width(&self) -> i32 {
        self.inner.width.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The current height (read by `view_rect`).
    pub(crate) fn height(&self) -> i32 {
        self.inner.height.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Update the size (the `Page` calls this on resize, then `was_resized()`).
    pub(crate) fn set(&self, width: i32, height: i32) {
        self.inner
            .width
            .store(width, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .height
            .store(height, std::sync::atomic::Ordering::SeqCst);
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
                    r.x = 0;
                    r.y = 0;
                    r.width = self.state.size.width();
                    r.height = self.state.size.height();
                }
            });
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
// ResourceRequestHandler / RequestHandler — the network interception seam.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct InterceptState {
    interceptor: Arc<dyn ResourceInterceptor>,
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
            _frame: Option<&mut cef::Frame>,
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

wrap_request_handler! {
    struct RequestHandlerImpl {
        state: InterceptState,
    }

    impl RequestHandler {
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            guard(None, || {
                Some(ResourceRequestHandlerImpl::new(InterceptState {
                    interceptor: Arc::clone(&self.state.interceptor),
                }))
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Client — wires the render, load, and request handlers onto a browser.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ClientState {
    render: RenderHandler,
    load: LoadHandler,
    request: RequestHandler,
    display: DisplayHandler,
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
    }
}

/// Builds a fully-wired CEF [`Client`] for an off-screen browser.
///
/// Returns the client plus the [`FrameSlot`], [`NavState`], and [`TitleSlot`]
/// the owning `Page` reads from. Keeping construction here keeps every `cef::`
/// handler type inside the FFI module.
pub(crate) fn build_client(
    size: ViewSize,
    interceptor: Arc<dyn ResourceInterceptor>,
) -> (Client, FrameSlot, NavState, TitleSlot, ViewSize) {
    let slot = FrameSlot::new();
    let nav = NavState::default();
    let title = TitleSlot::new();

    let render = RenderHandlerImpl::new(RenderState {
        slot: slot.clone(),
        size: size.clone(),
    });
    let load = LoadHandlerImpl::new(LoadState { nav: nav.clone() });
    let request = RequestHandlerImpl::new(InterceptState { interceptor });
    let display = DisplayHandlerImpl::new(DisplayState {
        title: title.clone(),
    });

    let client = ClientImpl::new(ClientState {
        render,
        load,
        request,
        display,
    });

    (client, slot, nav, title, size)
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
    use super::pixel_buf_len;

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
}
