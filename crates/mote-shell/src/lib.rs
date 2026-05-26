//! Browser composition root for Mote — the interactive-slice glue.
//!
//! This crate is the **composition root**: it owns the [`winit`] window + event
//! loop (the single OS event source, ADR-0004), brings up the [`mote_cef`]
//! engine with an external message pump, assembles the chrome assets from
//! [`mote_ui`] into a `mote://chrome` resource set, creates the privileged
//! chrome page (via the host bridge) and an untrusted content page, and feeds
//! both off-screen paint streams into [`mote_ui`]'s wgpu [`Compositor`]
//! (chrome-surrounds-content).
//!
//! # The four integration seams wired here
//!
//! 1. **CEF pump ↔ winit loop.** CEF runs with `external_message_pump = true`;
//!    the shell calls [`mote_cef::Engine::pump`] on every loop iteration (driven
//!    by a continuous `about_to_wait` → `request_redraw` cycle) so paint /
//!    network / navigation callbacks fire even with no OS events.
//! 2. **Compositor feed.** New chrome frames go to
//!    [`Compositor::update_chrome`]; new content frames go to
//!    [`Compositor::update_page`] with the viewport rect; [`Compositor::render`]
//!    composites each frame. Re-upload is dirty-tracked on CEF's `paint_count`.
//! 3. **Input routing** (plan §1.3). Mouse events hit-test the viewport rect:
//!    inside → the content page in page-local coords; outside → the chrome page
//!    in window coords. Keyboard goes to the logical focus owner (the chrome
//!    omnibox reports focus over the bridge).
//! 4. **The `navigate` op.** The chrome omnibox calls
//!    `window.mote.invoke("navigate", {url})`; the host-bridge op handler — which
//!    must be `Send + Sync` and so cannot hold the `!Send` [`mote_cef::Page`] —
//!    enqueues a [`ShellCommand`] the winit loop drains and applies to the
//!    content page on the pump thread.
//!
//! ## What is stubbed / deferred (honest scope)
//!
//! - **Provider-plugin navigation** (`ui:urlbar_provider`): only the *mechanism*
//!   is wired (omnibox → `navigate` op → `Page::load_url`). The urlbar provider
//!   plugin routing is Wave C (plan §8.3); the op accepts a URL directly.
//! - **Session persistence**: tabs are in-memory only (the slice is explicitly
//!   pre-persistence, plan §10 milestone).
//! - **Tab lifecycle / multi-tab switching**: a single content page is created;
//!   the texture-swap path exists in the compositor but only one content page is
//!   driven here.
//! - **Profiles**: the content page uses CEF's global request context; a
//!   `default` [`mote_cef::ProfileHandle`] is created to exercise the path but
//!   the slice does not require per-identity isolation.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mote_cef::{
    ButtonAction, ChromePageRequest, ChromeResources, Engine, EngineConfig, HostBridge, KeyAction,
    KeyInput, Modifiers, MouseButton, MousePosition, OpRegistry, OpResponse, Page, PageOptions,
    PageRole, chrome_url,
};
use mote_ui::{Compositor, PixelFormat, ViewportRect};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton as WinitMouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// The window's initial logical size in physical pixels (the slice does not
/// persist geometry — DESIGN §Session: the WM handles placement).
const INITIAL_WIDTH: u32 = 1280;
/// The window's initial height.
const INITIAL_HEIGHT: u32 = 800;

/// The chrome tab strip + sidebar reserve the left edge; the omnibox the top.
/// These are the page-viewport insets the shell uses until the chrome reports
/// its real `<main>` geometry over the bridge (Wave C). They mirror the default
/// layout: the sidebar (activity bar 36 + panel 280 = 316px) on the left.
const VIEWPORT_LEFT: u32 = 316;
/// The top-bar (omnibox) height reserved above the viewport.
const VIEWPORT_TOP: u32 = 44;

/// The start URL the content page loads. A `data:` URL renders without network,
/// so the slice is deterministic offline; pass a real URL on the command line
/// (the first non-flag argument) to navigate live.
const DEFAULT_START_URL: &str = "data:text/html,\
<html><body style='margin:0;background:%23204060;color:%23eaeaea;\
font:32px sans-serif;display:flex;align-items:center;justify-content:center;\
height:100vh'>mote — composited web content</body></html>";

/// A command produced by a host-bridge op (which is `Send + Sync`) and applied
/// by the winit loop on the pump thread (which owns the `!Send` pages).
#[derive(Debug, Clone)]
enum ShellCommand {
    /// Navigate the content page to `url` (the omnibox `navigate` op).
    Navigate(String),
    /// The chrome reported a focus-owner change (`chrome` ⇒ keyboard to the
    /// omnibox; otherwise to the focused content page).
    FocusOwner(FocusOwner),
}

/// Who owns keyboard input (plan §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusOwner {
    /// The chrome document (omnibox / a chrome control) has focus.
    Chrome,
    /// The focused content page has focus.
    Page,
}

/// A lock-free-ish command queue shared between the op handlers (any thread)
/// and the winit loop. Ops are infrequent (user actions), so a `Mutex` is fine.
type CommandQueue = Arc<Mutex<VecDeque<ShellCommand>>>;

/// Boot the browser shell: open one window, bring up CEF, render the chrome with
/// a real page composited inside, and run the event loop until the window
/// closes.
///
/// Call this from `mote-app`'s `main` **after** [`mote_cef::bootstrap_with_bridge`]
/// returned [`mote_cef::ProcessRole::Browser`] (the subprocess path must have
/// already exited).
///
/// # Errors
/// Returns a boxed error if the engine, the chrome bridge, or the content page
/// cannot be created, or if the winit event loop cannot start.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // CEF flags (e.g. `--ozone-platform=x11`) are passed through to CEF by the
    // bootstrap; the start URL is the first non-flag argument, if any.
    let start_url = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| DEFAULT_START_URL.to_string());

    // The engine: external pump (we drive it from the winit loop), sandbox off
    // for the headless/dev target (matches the mote-cef examples).
    let config = EngineConfig {
        no_sandbox: true,
        ..EngineConfig::default()
    };
    let engine = Engine::init(&config)?;

    // Serve the chrome assets from the privileged `mote://chrome` origin. The
    // host (this crate) supplies mote-ui's embedded assets; mote-cef serves them.
    engine.register_chrome_resources(build_chrome_resources());

    let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));

    // The closed op set the chrome may invoke. Each op enqueues a ShellCommand
    // (the handlers are Send + Sync and cannot hold the !Send Page).
    let registry = build_op_registry(&commands);

    // The privileged chrome page (loaded from mote://chrome/index.html) + the
    // bridge that carries the op router (the ONLY way to attach it).
    let chrome_req = ChromePageRequest::new(
        &chrome_url("index.html"),
        &PageOptions {
            width: INITIAL_WIDTH,
            height: INITIAL_HEIGHT,
            frame_rate: 60,
            role: PageRole::Chrome,
        },
    );
    let bridge = HostBridge::for_chrome(chrome_req, registry)?;

    // The untrusted content page, sized to the initial viewport region.
    let (vw, vh) = viewport_size(INITIAL_WIDTH, INITIAL_HEIGHT);
    let content = Page::new(
        &start_url,
        &PageOptions {
            width: vw,
            height: vh,
            frame_rate: 60,
            role: PageRole::Content,
        },
    )?;

    let mut app = ShellApp::new(engine, bridge, content, commands);
    let event_loop = EventLoop::new()?;
    // Poll: we want to pump CEF continuously, not only on OS events.
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Build the `mote://chrome` resource set from mote-ui's embedded assets. The
/// chrome HTML `@import`s tokens/base/components by relative path, so each must
/// be registered at the path the document references.
fn build_chrome_resources() -> ChromeResources {
    let css = "text/css; charset=utf-8";
    let mut res = ChromeResources::new()
        // index.html first → it becomes the directory index for `mote://chrome/`.
        .register("index.html", mote_ui::CHROME_HTML, "text/html; charset=utf-8")
        .register("host.js", mote_ui::HOST_JS, "text/javascript; charset=utf-8")
        .register("tokens.css", mote_ui::TOKENS_CSS, css)
        .register("base.css", mote_ui::BASE_CSS, css)
        .register(
            "assets/wordmark.svg",
            mote_ui::WORDMARK_SVG,
            "image/svg+xml",
        )
        .register("assets/mark.svg", mote_ui::MARK_SVG, "image/svg+xml");
    for (name, contents) in mote_ui::COMPONENT_CSS {
        res = res.register(format!("components/{name}.css"), *contents, css);
    }
    res
}

/// Build the closed op registry. Ops translate chrome intents into
/// [`ShellCommand`]s the winit loop applies (the handlers are `Send + Sync` and
/// must not capture the `!Send` pages).
fn build_op_registry(commands: &CommandQueue) -> OpRegistry {
    let nav_queue = Arc::clone(commands);
    let focus_queue = Arc::clone(commands);
    OpRegistry::new()
        .register("navigate", move |params: &str| {
            json_string_field(params, "url").map_or_else(
                || OpResponse::err(400, "navigate requires a string `url`"),
                |url| {
                    push(&nav_queue, ShellCommand::Navigate(url));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("focus_changed", move |params: &str| {
            let owner = match json_string_field(params, "owner").as_deref() {
                Some("chrome") => FocusOwner::Chrome,
                _ => FocusOwner::Page,
            };
            push(&focus_queue, ShellCommand::FocusOwner(owner));
            OpResponse::ok("{\"ok\":true}")
        })
}

/// Enqueue a command (poisoned-lock tolerant: a poisoned queue is recoverable).
fn push(queue: &CommandQueue, command: ShellCommand) {
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(command);
}

/// The winit application: owns the window, the CEF engine + pages, and the
/// compositor. All CEF objects stay on this (the event-loop) thread.
struct ShellApp {
    engine: Engine,
    bridge: HostBridge,
    content: Page,
    commands: CommandQueue,
    /// Created lazily in `resumed` (winit creates the window there).
    window: Option<Arc<Window>>,
    compositor: Option<Compositor>,
    /// Last chrome/content paint counts uploaded — re-upload only on a new one.
    chrome_paints: u64,
    content_paints: u64,
    /// Physical window size (chrome covers all of it; the page fills the viewport).
    width: u32,
    height: u32,
    /// Last cursor position in physical window pixels (for click routing).
    cursor: (i32, i32),
    /// Active keyboard modifiers (forwarded to CEF).
    modifiers: Modifiers,
    /// Who owns keyboard input.
    focus: FocusOwner,
    /// CEF needs a brief warm-up before the chrome's first paint; we log once.
    first_frame_logged: bool,
    /// When the window opened (for the warm-up log only).
    started: Instant,
}

impl std::fmt::Debug for ShellApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellApp")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("focus", &self.focus)
            .field("has_window", &self.window.is_some())
            .finish_non_exhaustive()
    }
}

impl ShellApp {
    fn new(engine: Engine, bridge: HostBridge, content: Page, commands: CommandQueue) -> Self {
        Self {
            engine,
            bridge,
            content,
            commands,
            window: None,
            compositor: None,
            chrome_paints: 0,
            content_paints: 0,
            width: INITIAL_WIDTH,
            height: INITIAL_HEIGHT,
            cursor: (0, 0),
            modifiers: Modifiers::NONE,
            focus: FocusOwner::Page,
            first_frame_logged: false,
            started: Instant::now(),
        }
    }

    /// The current page-viewport rect in physical pixels (the region the content
    /// texture composites into; chrome surrounds it). Until the chrome reports
    /// its real `<main>` geometry (Wave C) this is computed from the fixed insets.
    fn viewport_rect(&self) -> ViewportRect {
        let (vw, vh) = viewport_size(self.width, self.height);
        ViewportRect::new(
            px(VIEWPORT_LEFT),
            px(VIEWPORT_TOP),
            px(vw.max(1)),
            px(vh.max(1)),
        )
    }

    /// Drain the op command queue and apply each command on this (pump) thread.
    fn drain_commands(&mut self) {
        let drained: Vec<ShellCommand> = {
            let mut q = self
                .commands
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.drain(..).collect()
        };
        for command in drained {
            match command {
                ShellCommand::Navigate(url) => {
                    eprintln!("mote-shell: navigate -> {url}");
                    self.content.load_url(&url);
                }
                ShellCommand::FocusOwner(owner) => {
                    self.set_focus_owner(owner);
                }
            }
        }
    }

    /// Mirror logical focus into CEF so exactly one browser thinks it is focused.
    fn set_focus_owner(&mut self, owner: FocusOwner) {
        if owner == self.focus {
            return;
        }
        self.focus = owner;
        match owner {
            FocusOwner::Chrome => {
                self.content.send_focus(false);
                self.bridge.page().send_focus(true);
            }
            FocusOwner::Page => {
                self.bridge.page().send_focus(false);
                self.content.send_focus(true);
            }
        }
    }

    /// Upload any newly painted chrome/content frames into the compositor.
    ///
    /// Frames are gathered first (immutable borrows of the pages) and the
    /// dirty-tracking counters advanced, so the subsequent `&mut compositor`
    /// uploads don't alias the page reads.
    fn upload_frames(&mut self) {
        let viewport = self.viewport_rect();

        let chrome_count = self.bridge.page().paint_count();
        let chrome_frame = (chrome_count != self.chrome_paints)
            .then(|| self.bridge.page().latest_frame())
            .flatten();
        if chrome_frame.is_some() {
            self.chrome_paints = chrome_count;
        }

        let content_count = self.content.paint_count();
        let content_frame = (content_count != self.content_paints)
            .then(|| self.content.latest_frame())
            .flatten();
        if content_frame.is_some() {
            self.content_paints = content_count;
        }

        let Some(compositor) = self.compositor.as_mut() else {
            return;
        };
        if let Some(frame) = chrome_frame
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: chrome upload failed: {e}");
        }
        if let Some(frame) = content_frame
            && let Err(e) = compositor.update_page(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
                viewport,
            )
        {
            eprintln!("mote-shell: page upload failed: {e}");
        }
    }

    /// Resize: reconfigure the surface, tell both browsers their new sizes, and
    /// force a re-upload (the dirty-tracking counters are left; CEF repaints).
    fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.width = size.width;
        self.height = size.height;
        if let Some(compositor) = self.compositor.as_mut() {
            compositor.resize(size.width, size.height);
        }
        self.bridge.page().notify_resized(size.width, size.height);
        let (vw, vh) = viewport_size(size.width, size.height);
        self.content.notify_resized(vw, vh);
    }

    /// Route a mouse click to the content page (if inside the viewport) or the
    /// chrome page (otherwise), in the correct coordinate space (plan §1.3).
    fn route_click(&mut self, button: WinitMouseButton, state: ElementState) {
        let Some(mb) = map_mouse_button(button) else {
            return;
        };
        let action = match state {
            ElementState::Pressed => ButtonAction::Down,
            ElementState::Released => ButtonAction::Up,
        };
        let (x, y) = self.cursor;
        if let Some(pos) = self.page_local(x, y) {
            // Inside the page region → focus the page, inject page-local coords.
            self.set_focus_owner(FocusOwner::Page);
            self.content
                .send_mouse_button(pos, mb, action, 1, self.modifiers);
        } else {
            // Over the chrome → focus chrome, inject window coords.
            self.set_focus_owner(FocusOwner::Chrome);
            self.bridge.page().send_mouse_button(
                MousePosition { x, y },
                mb,
                action,
                1,
                self.modifiers,
            );
        }
    }

    /// Route a mouse move to whichever surface the cursor is over.
    fn route_mouse_move(&self) {
        let (x, y) = self.cursor;
        if let Some(pos) = self.page_local(x, y) {
            self.content.send_mouse_move(pos, self.modifiers, false);
        } else {
            self.bridge
                .page()
                .send_mouse_move(MousePosition { x, y }, self.modifiers, false);
        }
    }

    /// If window-space `(x, y)` is inside the page viewport, return the
    /// page-local position; otherwise `None` (the event belongs to the chrome).
    const fn page_local(&self, x: i32, y: i32) -> Option<MousePosition> {
        page_local_coords(x, y, self.width, self.height)
    }

    /// Route a keyboard event to the logical focus owner.
    fn route_key(&self, event: &winit::event::KeyEvent) {
        let target: &Page = match self.focus {
            FocusOwner::Chrome => self.bridge.page(),
            FocusOwner::Page => &self.content,
        };
        let action = match event.state {
            ElementState::Pressed => KeyAction::Down,
            ElementState::Released => KeyAction::Up,
        };
        let win_code = windows_key_code(&event.logical_key);
        target.send_key(KeyInput {
            action,
            windows_key_code: win_code,
            native_key_code: 0,
            character: 0,
            modifiers: self.modifiers,
        });
        // On press, also deliver the resolved character(s) as Char events so
        // text lands in the focused field.
        if event.state == ElementState::Pressed
            && let Some(text) = &event.text
        {
            for unit in text.encode_utf16() {
                target.send_key(KeyInput {
                    action: KeyAction::Char,
                    windows_key_code: win_code,
                    native_key_code: 0,
                    character: unit,
                    modifiers: self.modifiers,
                });
            }
        }
    }
}

impl ApplicationHandler for ShellApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("mote")
            .with_inner_size(PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("mote-shell: failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        self.width = size.width.max(1);
        self.height = size.height.max(1);

        match Compositor::new_for_window(Arc::clone(&window), self.width, self.height) {
            Ok(c) => self.compositor = Some(c),
            Err(e) => {
                eprintln!("mote-shell: failed to create compositor: {e}");
                event_loop.exit();
                return;
            }
        }

        // The content page renders into the viewport region; size it to match.
        let (vw, vh) = viewport_size(self.width, self.height);
        self.content.notify_resized(vw, vh);
        self.bridge.page().notify_resized(self.width, self.height);
        self.window = Some(window);
        eprintln!(
            "mote-shell: window {}x{} up; chrome + content pages live",
            self.width, self.height
        );
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                eprintln!("mote-shell: close requested; shutting down");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.handle_resize(size),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (cursor_px(position.x), cursor_px(position.y));
                self.route_mouse_move();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.route_click(button, state);
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = map_modifiers(mods.state());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.route_key(&event);
            }
            WindowEvent::RedrawRequested => {
                if let Some(compositor) = self.compositor.as_mut()
                    && let Err(e) = compositor.render()
                {
                    eprintln!("mote-shell: render failed: {e}");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // The heart of the integration: pump CEF, apply op commands, upload any
        // new frames, then request a redraw. Runs every loop iteration (Poll).
        self.engine.pump();
        self.drain_commands();
        self.upload_frames();

        if !self.first_frame_logged
            && self.bridge.page().paint_count() >= 1
            && self.content.paint_count() >= 1
        {
            self.first_frame_logged = true;
            eprintln!(
                "mote-shell: first chrome+content frames painted ({}ms after window)",
                self.started.elapsed().as_millis()
            );
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
        // A small sleep keeps the busy-poll from pegging a core while still
        // pumping CEF promptly (the spike's ~4ms cadence).
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Drop the pages before the engine shuts down (CEF needs a few pumps to tear
/// the hosts down). The winit loop has exited by the time this runs.
impl Drop for ShellApp {
    fn drop(&mut self) {
        self.content.close();
        self.bridge.page().close();
        for _ in 0..25 {
            self.engine.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The content page's surface size for a given window size (window minus the
/// chrome insets).
const fn viewport_size(width: u32, height: u32) -> (u32, u32) {
    let w = width.saturating_sub(VIEWPORT_LEFT);
    let h = height.saturating_sub(VIEWPORT_TOP);
    // `.max(1)` is not const on u32 until later; spell it out.
    (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
}

/// Convert a window/viewport pixel dimension to `f32` for the compositor's
/// `ViewportRect`. Window dimensions are far below `2^23` (f32's exact-integer
/// limit), so no precision is lost in practice.
#[allow(
    clippy::cast_precision_loss,
    reason = "window pixel dimensions are < 2^23; f32 represents them exactly"
)]
const fn px(v: u32) -> f32 {
    v as f32
}

/// Convert a winit physical cursor coordinate (`f64`) to integer window pixels.
#[allow(
    clippy::cast_possible_truncation,
    reason = "physical cursor coords are small positive pixel values; truncation toward the pixel is intended"
)]
const fn cursor_px(v: f64) -> i32 {
    v as i32
}

/// The window→page hit-test (plan §1.3). If `(x, y)` (window pixels) falls in
/// the page viewport region, return the page-local position; otherwise `None`
/// (the chrome owns the event). Pure so it is unit-testable without a live CEF.
const fn page_local_coords(x: i32, y: i32, width: u32, height: u32) -> Option<MousePosition> {
    let left = VIEWPORT_LEFT.cast_signed();
    let top = VIEWPORT_TOP.cast_signed();
    let right = width.cast_signed();
    let bottom = height.cast_signed();
    if x >= left && x < right && y >= top && y < bottom {
        Some(MousePosition {
            x: x - left,
            y: y - top,
        })
    } else {
        None
    }
}

/// Map a winit mouse button to the Mote vocabulary (other buttons unrouted).
const fn map_mouse_button(button: WinitMouseButton) -> Option<MouseButton> {
    match button {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Right => Some(MouseButton::Right),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Map winit modifier state to CEF event flags.
fn map_modifiers(state: winit::keyboard::ModifiersState) -> Modifiers {
    let mut m = Modifiers::NONE;
    if state.shift_key() {
        m |= Modifiers::SHIFT;
    }
    if state.control_key() {
        m |= Modifiers::CONTROL;
    }
    if state.alt_key() {
        m |= Modifiers::ALT;
    }
    if state.super_key() {
        m |= Modifiers::COMMAND;
    }
    m
}

/// A best-effort Windows virtual-key code for a winit logical key. Text input
/// rides on the `Char` events; this covers the navigation/control keys CEF needs
/// for caret movement, deletion, and Enter (the slice's omnibox usage).
fn windows_key_code(key: &Key) -> i32 {
    match key {
        Key::Named(NamedKey::Enter) => 0x0D,
        Key::Named(NamedKey::Backspace) => 0x08,
        Key::Named(NamedKey::Tab) => 0x09,
        Key::Named(NamedKey::Escape) => 0x1B,
        Key::Named(NamedKey::Space) => 0x20,
        Key::Named(NamedKey::ArrowLeft) => 0x25,
        Key::Named(NamedKey::ArrowUp) => 0x26,
        Key::Named(NamedKey::ArrowRight) => 0x27,
        Key::Named(NamedKey::ArrowDown) => 0x28,
        Key::Named(NamedKey::Delete) => 0x2E,
        Key::Named(NamedKey::Home) => 0x24,
        Key::Named(NamedKey::End) => 0x23,
        Key::Character(s) => s.chars().next().map_or(0, |c| {
            // ASCII letters/digits map to their uppercase code point (CEF's
            // cross-platform virtual-key code); non-ASCII rides on Char events.
            u8::try_from(u32::from(c.to_ascii_uppercase())).map_or(0, i32::from)
        }),
        _ => 0,
    }
}

/// Extract a top-level string field `"field": "value"` from a small JSON object.
/// The op params are chrome-bootstrap-authored JSON; a minimal parse suffices.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let after = after.strip_prefix('"')?;
    // Unescape only the minimal set the chrome bootstrap emits (\" and \\).
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            _ => out.push(c),
        }
    }
    None
}

/// A monotonically-increasing id source (reserved for the multi-tab path; the
/// slice drives a single content page). Kept so Wave C's `TabId → Page` map has
/// a seam to build on without reshaping `run`.
#[doc(hidden)]
pub fn next_tab_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_size_excludes_chrome_insets() {
        let (w, h) = viewport_size(1280, 800);
        assert_eq!(w, 1280 - VIEWPORT_LEFT);
        assert_eq!(h, 800 - VIEWPORT_TOP);
        // Degenerate window: never zero.
        assert_eq!(viewport_size(0, 0), (1, 1));
    }

    #[test]
    fn hit_test_maps_page_local_and_rejects_chrome() {
        // Inside the page region → page-local coords (window minus insets).
        let pos = page_local_coords(400, 300, 1280, 800).expect("inside viewport");
        assert_eq!(pos.x, 400 - VIEWPORT_LEFT.cast_signed());
        assert_eq!(pos.y, 300 - VIEWPORT_TOP.cast_signed());
        // Over the sidebar (left of the viewport) → chrome owns it.
        assert!(page_local_coords(100, 300, 1280, 800).is_none());
        // Over the omnibox (above the viewport) → chrome owns it.
        assert!(page_local_coords(400, 10, 1280, 800).is_none());
    }

    #[test]
    fn json_string_field_extracts_url() {
        assert_eq!(
            json_string_field(r#"{"url":"https://example.com"}"#, "url").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            json_string_field(r#"{"owner":"chrome"}"#, "owner").as_deref(),
            Some("chrome")
        );
        assert!(json_string_field(r#"{"n":3}"#, "url").is_none());
    }

    #[test]
    fn json_string_field_unescapes_quotes() {
        assert_eq!(
            json_string_field(r#"{"url":"a\"b"}"#, "url").as_deref(),
            Some("a\"b")
        );
    }

    #[test]
    fn enter_key_maps_to_carriage_return() {
        assert_eq!(windows_key_code(&Key::Named(NamedKey::Enter)), 0x0D);
        assert_eq!(
            windows_key_code(&Key::Character("a".into())),
            i32::from(b'A')
        );
    }

    #[test]
    fn tab_ids_are_monotonic() {
        let a = next_tab_id();
        let b = next_tab_id();
        assert!(b > a);
    }
}
