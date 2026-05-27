//! Browser composition root for Mote — the multi-tab browser shell.
//!
//! This crate is the **composition root**: it owns the [`winit`] window + event
//! loop (the single OS event source, ADR-0004), brings up the [`mote_cef`]
//! engine with an external message pump, assembles the chrome assets from
//! [`mote_ui`] into a `mote://chrome` resource set, creates the privileged
//! chrome page (via the host bridge) and a set of untrusted content pages (one
//! per tab, under a per-identity profile), and feeds the active page's
//! off-screen paint stream into [`mote_ui`]'s wgpu [`Compositor`]
//! (chrome-surrounds-content).
//!
//! # The integration seams wired here
//!
//! 1. **CEF pump ↔ winit loop.** CEF runs with `external_message_pump = true`;
//!    the shell calls [`mote_cef::Engine::pump`] on every loop iteration so
//!    paint / network / navigation callbacks fire even with no OS events.
//! 2. **Compositor feed.** New chrome frames go to
//!    [`Compositor::update_chrome`]; the **active tab's** new content frames go
//!    to [`Compositor::update_page`]. Switching the active tab swaps which
//!    page's frames feed the page texture (a re-upload of the new active page).
//! 3. **Multi-tab.** A vector of [`ShellTab`]s keyed by [`mote_types::TabId`]. Open
//!    creates a new content [`mote_cef::Page`] under the default
//!    [`mote_cef::ProfileHandle`]; close drops it; switching feeds the new
//!    active page. Inactive pages keep their CEF browsers (discard-on-idle is a
//!    later session concern); restored placeholders have **no** live page and
//!    materialize on first focus (the restoration model).
//! 4. **Per-identity profiles.** Content pages are created via
//!    [`mote_cef::ProfileManager`] under the `default` identity, so identity
//!    isolation (cookies/storage/cache) is actually in use — not the global
//!    request context.
//! 5. **Bridge → chrome state push.** On open / close / switch / navigate the
//!    shell pushes the live tab list (`set_tabs`) and current URL (`set_url`)
//!    into the chrome via [`mote_cef::Page::eval_js`] →
//!    `window.mote.applyOp(op, payload)`, so the chrome tab strip + omnibox
//!    reflect runtime state (replacing the static demo tabs).
//! 6. **Session persistence.** A [`mote_session::Session`] backed by per-identity
//!    [`mote_storage`] `SQLite`. The shell flushes on every tab change and on
//!    clean shutdown, and [`mote_session::Session::restore`]s on launch —
//!    restored active-workspace tabs become placeholders that load on focus
//!    (crash-recovery == clean-exit).
//! 7. **Input routing** (plan §1.3). Mouse events hit-test the viewport rect:
//!    inside → the active content page in page-local coords; outside → the
//!    chrome page in window coords. Keyboard goes to the logical focus owner.
//! 8. **The `navigate` op + tab ops.** The chrome invokes `navigate`, `new_tab`,
//!    `close_tab`, `select_tab`; each op (which is `Send + Sync` and cannot hold
//!    the `!Send` [`mote_cef::Page`]) enqueues a [`ShellCommand`] the winit loop
//!    drains and applies on the pump thread.
//!
//! ## What is stubbed / deferred (honest scope)
//!
//! - **Title tracking.** `mote-cef` does not yet surface a page-title callback,
//!   so a tab's title falls back to its URL. The push contract carries a `title`
//!   field already, so wiring a real title source later is additive.
//! - **Workspaces / multi-window.** A single window on workspace 0 under the
//!   `default` identity. The session model supports more; the shell drives one.
//! - **Discard / hidden-tab aging.** Inactive tabs keep their renderers; the
//!   `Discarder` / `HiddenTabReaper` integration is a later session concern.
//! - **Provider-plugin navigation** (`ui:urlbar_provider`): the op accepts a URL
//!   directly (the chrome bootstrap normalizes omnibox text).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mote_cef::{
    ButtonAction, ChromePageRequest, ChromeResources, Engine, EngineConfig, HostBridge, IdentityId,
    KeyAction, KeyInput, Modifiers, MouseButton, MousePosition, OpRegistry, OpResponse, Page,
    PageOptions, PageRole, ProfileHandle, ProfileManager, chrome_url,
};
use mote_session::Session;
use mote_storage::Store;
use mote_types::{TabId, WorkspaceId};
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
/// its real `<main>` geometry over the bridge (later). They mirror the default
/// layout: the sidebar (activity bar 36 + panel 280 = 316px) on the left.
const VIEWPORT_LEFT: u32 = 316;
/// The top-bar (omnibox) height reserved above the viewport.
const VIEWPORT_TOP: u32 = 44;

/// The identity every tab opens under in this slice (DESIGN: a single `default`
/// identity hides the multi-identity machinery until config creates more).
const DEFAULT_IDENTITY: &str = "default";

/// The session-storage identity id (the `mote-session` integer identity axis).
/// One identity in this slice, so a fixed id is fine.
const SESSION_IDENTITY: u64 = 0;

/// The single workspace this slice drives.
const WORKSPACE: u64 = 0;

/// The start URL a brand-new tab loads. A `data:` URL renders without network,
/// so the slice is deterministic offline; pass a real URL on the command line
/// (the first non-flag argument) to open the first tab live.
const DEFAULT_START_URL: &str = "data:text/html,\
<html><body style='margin:0;background:%23204060;color:%23eaeaea;\
font:32px sans-serif;display:flex;align-items:center;justify-content:center;\
height:100vh'>mote — composited web content</body></html>";

/// A command produced by a host-bridge op (which is `Send + Sync`) and applied
/// by the winit loop on the pump thread (which owns the `!Send` pages).
#[derive(Debug, Clone)]
enum ShellCommand {
    /// Navigate the **active** tab to `url` (the omnibox `navigate` op).
    Navigate(String),
    /// Open a new tab (and switch to it).
    NewTab,
    /// Close the tab with this id.
    CloseTab(u64),
    /// Switch the active tab to this id.
    SelectTab(u64),
    /// The chrome reported a focus-owner change (`chrome` ⇒ keyboard to the
    /// omnibox; otherwise to the active content page).
    FocusOwner(FocusOwner),
}

/// Who owns keyboard input (plan §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusOwner {
    /// The chrome document (omnibox / a chrome control) has focus.
    Chrome,
    /// The active content page has focus.
    Page,
}

/// A lock-free-ish command queue shared between the op handlers (any thread)
/// and the winit loop. Ops are infrequent (user actions), so a `Mutex` is fine.
type CommandQueue = Arc<Mutex<VecDeque<ShellCommand>>>;

/// One tab the shell owns: its stable id, its current URL/title, and its live
/// CEF [`Page`] — `None` for a **placeholder** (a restored tab that has not yet
/// been focused; it materializes its page on first selection, per the
/// restoration model).
struct ShellTab {
    id: TabId,
    url: String,
    title: Option<String>,
    page: Option<Page>,
}

impl ShellTab {
    /// `true` if this tab has a live CEF page (vs a restore placeholder).
    const fn is_live(&self) -> bool {
        self.page.is_some()
    }
}

/// Boot the browser shell: open one window, bring up CEF, restore (or seed) the
/// session, render the chrome with the active tab composited inside, and run the
/// event loop until the window closes.
///
/// Call this from `mote-app`'s `main` **after** [`mote_cef::bootstrap_with_bridge`]
/// returned [`mote_cef::ProcessRole::Browser`].
///
/// # Errors
/// Returns a boxed error if the engine, the chrome bridge, the session store, or
/// the first content page cannot be created, or if the winit loop cannot start.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // CEF flags (e.g. `--ozone-platform=x11`) are passed through to CEF by the
    // bootstrap; every non-flag argument is an initial tab URL (like
    // `firefox url1 url2 …`). With none, a fresh session seeds one default tab.
    let cli_urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();

    let cache_path = default_cache_path();
    let config = EngineConfig {
        no_sandbox: true,
        cache_path: cache_path.clone(),
        ..EngineConfig::default()
    };
    let engine = Engine::init(&config)?;

    // Serve the chrome assets from the privileged `mote://chrome` origin.
    engine.register_chrome_resources(build_chrome_resources());

    // Per-identity profiles MUST be rooted at the engine's cache path (CEF
    // requires each profile dir to be a direct child of root_cache_path).
    let profiles = ProfileManager::new(cache_path);
    let default_identity = IdentityId::new(DEFAULT_IDENTITY)?;
    let default_profile = profiles.get_or_create(&default_identity)?;
    // A freshly created profile context initialises asynchronously on the CEF UI
    // thread; pump a few times before creating the first page under it.
    for _ in 0..20 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(5));
    }

    // Session: per-identity SQLite, restored on launch (empty on first run).
    let store = open_session_store()?;
    let session_identity = mote_types::IdentityId::new(SESSION_IDENTITY);
    let ns = Session::open_namespace(&store, session_identity)?;
    let mut session = Session::restore(&ns, session_identity)?;

    let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
    let registry = build_op_registry(&commands);

    // The privileged chrome page + the bridge that carries the op router.
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

    // Build the tab set from the restored session (active-workspace tabs become
    // placeholders), or seed a first tab if the session is empty.
    let workspace = WorkspaceId::new(WORKSPACE);
    let (vw, vh) = viewport_size(INITIAL_WIDTH, INITIAL_HEIGHT);
    let content_opts = PageOptions {
        width: vw,
        height: vh,
        frame_rate: 60,
        role: PageRole::Content,
    };

    let active: usize = 0;
    let tabs = build_initial_tabs(
        &mut session,
        &ns,
        workspace,
        &content_opts,
        &default_profile,
        cli_urls,
    )?;

    let mut app = ShellApp {
        engine,
        bridge,
        profiles,
        default_profile,
        content_opts,
        session,
        store,
        session_identity,
        workspace,
        tabs,
        active,
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
        chrome_ready: false,
        first_frame_logged: false,
        started: Instant::now(),
    };

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Build the shell's initial tab set: restore the active workspace's tabs as
/// placeholders (they load on focus), or — on a fresh session — seed one tab per
/// CLI URL (the first live, the rest placeholders) and flush. The active tab is
/// index 0; its page is always materialized so the window is not blank.
fn build_initial_tabs(
    session: &mut Session,
    ns: &mote_storage::Namespace,
    workspace: WorkspaceId,
    content_opts: &PageOptions,
    default_profile: &ProfileHandle,
    cli_urls: Vec<String>,
) -> Result<Vec<ShellTab>, Box<dyn std::error::Error>> {
    let mut tabs: Vec<ShellTab> = session
        .tab_picker_ranked(workspace)
        .into_iter()
        .map(|tab| ShellTab {
            id: tab.id,
            url: tab.url.clone(),
            title: tab.title.clone(),
            page: None,
        })
        .collect();

    if tabs.is_empty() {
        let urls = if cli_urls.is_empty() {
            vec![DEFAULT_START_URL.to_string()]
        } else {
            cli_urls
        };
        for (i, url) in urls.into_iter().enumerate() {
            let id = session.add_tab(url.clone(), workspace);
            let page = if i == 0 {
                Some(Page::with_profile(&url, content_opts, default_profile)?)
            } else {
                None
            };
            tabs.push(ShellTab {
                id,
                url,
                title: None,
                page,
            });
        }
        session.flush(ns)?;
    } else {
        // Restored: materialize the active tab eagerly so the window is not blank.
        let url = tabs[0].url.clone();
        if let Ok(page) = Page::with_profile(&url, content_opts, default_profile) {
            tabs[0].page = Some(page);
        }
    }
    Ok(tabs)
}

/// The CEF cache/profile root. Per DESIGN, per-identity storage lives under the
/// XDG state dir; the engine cache path doubles as the profile-manager root.
fn default_cache_path() -> PathBuf {
    state_dir().join("cef-cache")
}

/// Open the per-identity session `SQLite` store under the XDG state dir
/// (`~/.local/state/mote/session.db`), creating the directory if needed.
fn open_session_store() -> Result<Store, Box<dyn std::error::Error>> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(Store::open(dir.join("session.db"))?)
}

/// `~/.local/state/mote` (honouring `XDG_STATE_HOME`), or `.mote-state` under the
/// cwd as a last resort.
fn state_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("mote");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/mote");
    }
    PathBuf::from(".mote-state")
}

/// Build the `mote://chrome` resource set from mote-ui's embedded assets.
fn build_chrome_resources() -> ChromeResources {
    let css = "text/css; charset=utf-8";
    let mut res = ChromeResources::new()
        .register(
            "index.html",
            mote_ui::CHROME_HTML,
            "text/html; charset=utf-8",
        )
        .register(
            "host.js",
            mote_ui::HOST_JS,
            "text/javascript; charset=utf-8",
        )
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
    let new_queue = Arc::clone(commands);
    let close_queue = Arc::clone(commands);
    let select_queue = Arc::clone(commands);
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
        .register("new_tab", move |_params: &str| {
            push(&new_queue, ShellCommand::NewTab);
            OpResponse::ok("{\"ok\":true}")
        })
        .register("close_tab", move |params: &str| {
            json_u64_field(params, "id").map_or_else(
                || OpResponse::err(400, "close_tab requires a numeric `id`"),
                |id| {
                    push(&close_queue, ShellCommand::CloseTab(id));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("select_tab", move |params: &str| {
            json_u64_field(params, "id").map_or_else(
                || OpResponse::err(400, "select_tab requires a numeric `id`"),
                |id| {
                    push(&select_queue, ShellCommand::SelectTab(id));
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

/// The winit application: owns the window, the CEF engine + pages, the
/// compositor, and the session. All CEF objects stay on this (the event-loop)
/// thread.
struct ShellApp {
    engine: Engine,
    bridge: HostBridge,
    /// Interns one profile per identity; the default identity's profile backs
    /// every tab in this slice.
    #[allow(
        dead_code,
        reason = "retained so the default profile's manager outlives the pages"
    )]
    profiles: ProfileManager,
    default_profile: ProfileHandle,
    /// Page geometry/role for a new content page (recomputed on resize).
    content_opts: PageOptions,
    session: Session,
    #[allow(
        dead_code,
        reason = "retained so the session namespace's connection stays open"
    )]
    store: Store,
    session_identity: mote_types::IdentityId,
    workspace: WorkspaceId,
    /// The open tabs in display order; `active` indexes the focused one.
    tabs: Vec<ShellTab>,
    active: usize,
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
    /// `true` once the chrome has painted at least once, so it can receive its
    /// initial tab-list / URL push (an `applyOp` before the bootstrap runs is
    /// lost).
    chrome_ready: bool,
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
            .field("tabs", &self.tabs.len())
            .field("active", &self.active)
            .field("focus", &self.focus)
            .field("has_window", &self.window.is_some())
            .finish_non_exhaustive()
    }
}

impl ShellApp {
    /// The current page-viewport rect in physical pixels.
    fn viewport_rect(&self) -> ViewportRect {
        let (vw, vh) = viewport_size(self.width, self.height);
        ViewportRect::new(
            px(VIEWPORT_LEFT),
            px(VIEWPORT_TOP),
            px(vw.max(1)),
            px(vh.max(1)),
        )
    }

    /// The active tab's live page, if any (a placeholder has none).
    fn active_page(&self) -> Option<&Page> {
        self.tabs.get(self.active).and_then(|t| t.page.as_ref())
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
                ShellCommand::Navigate(url) => self.navigate_active(&url),
                ShellCommand::NewTab => self.open_tab(None),
                ShellCommand::CloseTab(id) => self.close_tab(TabId::new(id)),
                ShellCommand::SelectTab(id) => self.select_tab(TabId::new(id)),
                ShellCommand::FocusOwner(owner) => self.set_focus_owner(owner),
            }
        }
    }

    /// Navigate the active tab, update session state, and push the new URL.
    fn navigate_active(&mut self, url: &str) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        eprintln!("mote-shell: navigate tab {} -> {url}", tab.id);
        tab.url = url.to_string();
        tab.title = None;
        if let Some(page) = tab.page.as_ref() {
            page.load_url(url);
        }
        if let Some(stab) = self.session.tab_mut(tab.id) {
            stab.url = url.to_string();
            stab.title = None;
        }
        self.persist_and_push();
    }

    /// Open a new tab (live page under the default profile) and switch to it.
    /// `url` defaults to the start URL.
    fn open_tab(&mut self, url: Option<String>) {
        let url = url.unwrap_or_else(|| DEFAULT_START_URL.to_string());
        let id = self.session.add_tab(url.clone(), self.workspace);
        let page = match Page::with_profile(&url, &self.content_opts, &self.default_profile) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: failed to create page for new tab: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url,
            title: None,
            page,
        });
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Close the tab with `id`. The session marks it closed; the live page (if
    /// any) is dropped (its CEF browser tears down). If the active tab closes,
    /// focus moves to its neighbour. Closing the last tab opens a fresh one.
    fn close_tab(&mut self, id: TabId) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        eprintln!("mote-shell: close tab {id}");
        let removed = self.tabs.remove(idx);
        if let Some(page) = removed.page {
            page.close();
        }
        let _ = self.session.close_tab(id);

        if self.tabs.is_empty() {
            // Never leave the window tab-less: open a fresh tab.
            self.open_tab(None);
            return;
        }
        // Keep the active index valid and pointing at a sensible neighbour.
        if idx <= self.active {
            self.active = self.active.saturating_sub(1);
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Switch the active tab to `id`. A placeholder materializes its page here
    /// (load-on-focus, the restoration model).
    fn select_tab(&mut self, id: TabId) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        if idx == self.active && self.tabs[idx].is_live() {
            return;
        }
        self.active = idx;
        // Materialize a placeholder on focus.
        if !self.tabs[idx].is_live() {
            let url = self.tabs[idx].url.clone();
            eprintln!("mote-shell: materialize placeholder tab {id} -> {url}");
            match Page::with_profile(&url, &self.content_opts, &self.default_profile) {
                Ok(page) => self.tabs[idx].page = Some(page),
                Err(e) => eprintln!("mote-shell: failed to materialize tab {id}: {e}"),
            }
        }
        self.on_active_changed();
        self.persist_and_push();
    }

    /// React to a change of active tab: size the new page to the viewport, give
    /// it focus, and force a content re-upload (so the compositor swaps to it).
    fn on_active_changed(&mut self) {
        let (vw, vh) = viewport_size(self.width, self.height);
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh);
        }
        // Reset the content dirty counter so the next upload_frames re-uploads
        // the new active page's frame (texture swap).
        self.content_paints = 0;
        // The freshly focused page should believe it has focus if the page owns
        // input; otherwise focus stays with the chrome.
        if self.focus == FocusOwner::Page
            && let Some(page) = self.active_page()
        {
            page.send_focus(true);
        }
    }

    /// Flush the session and push the live tab list + active URL to the chrome.
    fn persist_and_push(&self) {
        if let Some(ns) = self.session_namespace()
            && let Err(e) = self.session.flush(&ns)
        {
            eprintln!("mote-shell: session flush failed: {e}");
        }
        self.push_state_to_chrome();
    }

    /// Reopen the per-identity session namespace (cheap; just binds the shared
    /// connection + scope).
    fn session_namespace(&self) -> Option<mote_storage::Namespace> {
        Session::open_namespace(&self.store, self.session_identity).ok()
    }

    /// Push the current tab list (`set_tabs`) and active URL (`set_url`) into the
    /// chrome document via the privileged `window.mote.applyOp` seam.
    fn push_state_to_chrome(&self) {
        if !self.chrome_ready {
            return;
        }
        let tabs_json = self.tabs_json();
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_tabs',{{tabs:{tabs_json}}});"
        ));
        if let Some(tab) = self.tabs.get(self.active) {
            let url = js_string(&tab.url);
            chrome.eval_js(&format!(
                "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_url',{{url:{url}}});"
            ));
        }
    }

    /// Serialise the tab list to a JSON array `[ {id,title,url,active}, … ]` with
    /// every string JS-escaped (these are page-derived; the chrome inserts them
    /// as text nodes, never markup — bridge.rs caller discipline).
    fn tabs_json(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::from("[");
        for (i, tab) in self.tabs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let title = tab.title.clone().unwrap_or_else(|| tab.url.clone());
            let _ = write!(
                out,
                "{{\"id\":{},\"title\":{},\"url\":{},\"active\":{}}}",
                tab.id.get(),
                js_string(&title),
                js_string(&tab.url),
                i == self.active
            );
        }
        out.push(']');
        out
    }

    /// Mirror logical focus into CEF so exactly one browser thinks it is focused.
    fn set_focus_owner(&mut self, owner: FocusOwner) {
        if owner == self.focus {
            return;
        }
        self.focus = owner;
        match owner {
            FocusOwner::Chrome => {
                if let Some(page) = self.active_page() {
                    page.send_focus(false);
                }
                self.bridge.page().send_focus(true);
            }
            FocusOwner::Page => {
                self.bridge.page().send_focus(false);
                if let Some(page) = self.active_page() {
                    page.send_focus(true);
                }
            }
        }
    }

    /// Upload any newly painted chrome / active-content frames into the
    /// compositor.
    fn upload_frames(&mut self) {
        let viewport = self.viewport_rect();

        let chrome_count = self.bridge.page().paint_count();
        let chrome_frame = (chrome_count != self.chrome_paints)
            .then(|| self.bridge.page().latest_frame())
            .flatten();
        if chrome_frame.is_some() {
            self.chrome_paints = chrome_count;
        }

        let content = self.active_page();
        let content_count = content.map_or(0, Page::paint_count);
        let content_frame = content.and_then(|p| {
            (content_count != self.content_paints)
                .then(|| p.latest_frame())
                .flatten()
        });
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

    /// Resize: reconfigure the surface, tell the chrome + active page their new
    /// sizes, and force a re-upload.
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
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh);
        }
    }

    /// Route a mouse click to the active content page (if inside the viewport) or
    /// the chrome page (otherwise), in the correct coordinate space (plan §1.3).
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
            self.set_focus_owner(FocusOwner::Page);
            if let Some(page) = self.active_page() {
                page.send_mouse_button(pos, mb, action, 1, self.modifiers);
            }
        } else {
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
            if let Some(page) = self.active_page() {
                page.send_mouse_move(pos, self.modifiers, false);
            }
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

    /// Intercept the chrome keybinds that must always win before the page sees
    /// the key (plan §1.3 / §6.1): `Ctrl+T` new tab, `Ctrl+W` close active tab,
    /// `Ctrl+Tab` next tab. Returns `true` if the key was consumed (not routed).
    ///
    /// Uses `Ctrl` (the Linux/dev convention) where the spec writes `⌘`.
    fn intercept_keybind(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state != ElementState::Pressed || !self.modifiers.contains(Modifiers::CONTROL) {
            return false;
        }
        match &event.logical_key {
            Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                self.open_tab(None);
                true
            }
            Key::Character(s) if s.eq_ignore_ascii_case("w") => {
                if let Some(tab) = self.tabs.get(self.active) {
                    let id = tab.id;
                    self.close_tab(id);
                }
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_active_tab();
                true
            }
            _ => false,
        }
    }

    /// Advance the active tab to the next one (wrapping), materializing a
    /// placeholder on focus. No-op with fewer than two tabs.
    fn cycle_active_tab(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        let next = (self.active + 1) % self.tabs.len();
        let id = self.tabs[next].id;
        self.select_tab(id);
    }

    /// Route a keyboard event to the logical focus owner.
    fn route_key(&self, event: &winit::event::KeyEvent) {
        let target: Option<&Page> = match self.focus {
            FocusOwner::Chrome => Some(self.bridge.page()),
            FocusOwner::Page => self.active_page(),
        };
        let Some(target) = target else {
            return;
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

        let (vw, vh) = viewport_size(self.width, self.height);
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh);
        }
        self.bridge.page().notify_resized(self.width, self.height);
        self.window = Some(window);
        eprintln!(
            "mote-shell: window {}x{} up; chrome + {} tab(s) live",
            self.width,
            self.height,
            self.tabs.len()
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
            // Chrome keybinds win before the page sees the key (plan §1.3 /
            // §6.1). Only un-intercepted keys route to the focus owner.
            WindowEvent::KeyboardInput { event, .. } if !self.intercept_keybind(&event) => {
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
        self.engine.pump();
        self.drain_commands();
        self.upload_frames();

        // Once the chrome has painted, its bootstrap has run; push the initial
        // tab list + URL exactly once (an applyOp before then would be lost).
        if !self.chrome_ready && self.bridge.page().paint_count() >= 1 {
            self.chrome_ready = true;
            self.push_state_to_chrome();
        }

        if !self.first_frame_logged
            && self.bridge.page().paint_count() >= 1
            && self.active_page().is_some_and(|p| p.paint_count() >= 1)
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
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// On exit: flush the session (clean-exit == crash-recovery), close every page +
/// the chrome, then pump so CEF can tear the hosts down before shutdown.
impl Drop for ShellApp {
    fn drop(&mut self) {
        if let Some(ns) = self.session_namespace()
            && let Err(e) = self.session.flush(&ns)
        {
            eprintln!("mote-shell: final session flush failed: {e}");
        }
        for tab in &self.tabs {
            if let Some(page) = tab.page.as_ref() {
                page.close();
            }
        }
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
    (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
}

/// Convert a window/viewport pixel dimension to `f32` for the compositor's
/// `ViewportRect`.
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

/// The window→page hit-test (plan §1.3).
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

/// A best-effort Windows virtual-key code for a winit logical key.
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
            u8::try_from(u32::from(c.to_ascii_uppercase())).map_or(0, i32::from)
        }),
        _ => 0,
    }
}

/// Quote and escape `s` as a JavaScript string literal (double-quoted). Escapes
/// the control / quote / backslash / line-terminator set so a page-derived URL
/// or title can never break out of the literal or inject script when the chrome
/// `eval_js`'s `window.mote.applyOp(...)` call is built.
fn js_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // JS string literals may not contain these raw line terminators.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            // Escape '<' so a "</script" sequence cannot terminate any
            // surrounding script context defensively.
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Extract a top-level string field `"field": "value"` from a small JSON object.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let after = after.strip_prefix('"')?;
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

/// Extract a top-level numeric field `"field": <number>` from a small JSON
/// object (the tab-op `id`s the chrome bootstrap sends are integers).
fn json_u64_field(json: &str, field: &str) -> Option<u64> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_size_excludes_chrome_insets() {
        let (w, h) = viewport_size(1280, 800);
        assert_eq!(w, 1280 - VIEWPORT_LEFT);
        assert_eq!(h, 800 - VIEWPORT_TOP);
        assert_eq!(viewport_size(0, 0), (1, 1));
    }

    #[test]
    fn hit_test_maps_page_local_and_rejects_chrome() {
        let pos = page_local_coords(400, 300, 1280, 800).expect("inside viewport");
        assert_eq!(pos.x, 400 - VIEWPORT_LEFT.cast_signed());
        assert_eq!(pos.y, 300 - VIEWPORT_TOP.cast_signed());
        assert!(page_local_coords(100, 300, 1280, 800).is_none());
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
    fn json_u64_field_extracts_id() {
        assert_eq!(json_u64_field(r#"{"id":42}"#, "id"), Some(42));
        assert_eq!(json_u64_field(r#"{"id": 7 ,"x":1}"#, "id"), Some(7));
        assert!(json_u64_field(r#"{"id":"nope"}"#, "id").is_none());
        assert!(json_u64_field(r#"{"other":1}"#, "id").is_none());
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
    fn js_string_escapes_injection_vectors() {
        assert_eq!(js_string("ab"), "\"ab\"");
        assert_eq!(js_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(js_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(js_string("a\nb"), "\"a\\nb\"");
        // A `</script>` payload cannot terminate a script context.
        assert!(!js_string("</script>").contains("</script>"));
    }
}
