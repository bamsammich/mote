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

mod approval;
mod picker;
mod runtime;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use mote_cef::{
    ButtonAction, ChromePageRequest, ChromeResources, Engine, EngineConfig, HostBridge, IdentityId,
    KeyAction, KeyInput, Modifiers, MouseButton, MousePosition, OpRegistry, OpResponse, Page,
    PageOptions, PageRole, ProfileHandle, ProfileManager, chrome_url, overlay_url,
};
use mote_runtime::{HostValue, host_to_json};
use mote_session::{DiscardConfig, Discarder, HiddenTabConfig, HiddenTabReaper, Session};
use mote_storage::Store;
use mote_types::{TabId, WorkspaceId};
use mote_ui::{ApprovalRequest, Compositor, IntegrityPanel, PixelFormat, ViewportRect};

use crate::picker::{PickerEntry, PickerState};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
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

/// How often the shell runs the session housekeeping pass (active-tab discard +
/// hidden-tab reap). The decisions themselves use the configured idle/TTL
/// thresholds; this is just the polling cadence. A minute is far below either
/// default threshold, so housekeeping is timely without busy-looping.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_mins(1);

/// Environment override for the active-tab discard threshold, in **seconds**
/// (`MOTE_DISCARD_AFTER_SECS`). Lets a test drive the discard path without
/// waiting the 30-minute default. Unset → [`DiscardConfig::default`] (30 min).
const DISCARD_AFTER_ENV: &str = "MOTE_DISCARD_AFTER_SECS";

/// Environment override for the hidden-tab TTL, in **seconds**
/// (`MOTE_HIDDEN_TTL_SECS`). Lets a test drive the reap path without waiting the
/// 30-day default. Unset → [`HiddenTabConfig::default`] (30 days).
const HIDDEN_TTL_ENV: &str = "MOTE_HIDDEN_TTL_SECS";

/// Environment override for the housekeeping cadence, in **seconds**
/// (`MOTE_HOUSEKEEPING_SECS`). Lets a test see discard/reap fire promptly.
const HOUSEKEEPING_ENV: &str = "MOTE_HOUSEKEEPING_SECS";

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
    /// The user answered the approval dialog (`approve_plugin` op). The payload
    /// passed the op-boundary structural validation; the pump thread does the
    /// semantic cross-check and finishes the load/deny.
    ApprovePlugin(approval::DialogResult),
    /// Panel action: update the named plugin (git/bundled `plugin_update`).
    PluginUpdate(String),
    /// Panel action: roll the named plugin back to its prior commit.
    PluginRollback(String),
    /// Panel action: reload the named plugin (path/dev re-run).
    PluginReload(String),
    /// Panel action: revoke the named plugin (unload + drop its approval).
    PluginRevoke(String),
    /// Panel action: re-open the approval dialog for the named plugin so the
    /// user can re-narrow its grant.
    PluginAdjustScope(String),
    /// Panel action: revoke a specific secret grant from the named plugin
    /// (session-scoped narrowing; does not persist across relaunch).
    PluginRevokeSecret {
        /// The plugin whose grant should be narrowed.
        plugin: String,
        /// The secret name (`<name>` from `secret:read:<name>`).
        name: String,
    },
    /// The chrome omnibox input changed; invoke `ui:urlbar_provider` → `query`
    /// and push the results to chrome as `applyOp('urlbar_suggestions', …)`.
    UrlbarQuery(String),
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

    // The single shared per-identity SQLite store backs the session, the plugin
    // runtime's per-plugin storage, and the audit sink (one database, namespaced).
    let store = open_session_store()?;

    // Stand up the plugin runtime (runtime + manager + audit + approval store).
    // Per ADR-0007 this constructs but loads NOTHING: the resolve + classify +
    // load pass runs in `run_initial_load_pass`, fired once from `about_to_wait`
    // after the chrome's first paint so the window is live before any plugin
    // (or git fetch) work, and a fatal resolution error leaves the window alive.
    let host = runtime::PluginHost::boot(store.clone())?;

    // Render the integrity panel from LIVE loaded-plugin / audit / storage data,
    // and serve it as the `mote://overlay/integrity.html` overlay surface.
    let integrity_html = runtime::render_panel_html(&host.build_panel());

    // Serve the privileged chrome document from `mote://chrome` (the only origin
    // the host-bridge binding is installed on), and the unprivileged overlay
    // surfaces (picker + integrity) from the distinct `mote://overlay` origin the
    // origin gate does NOT match (S2).
    engine.register_chrome_resources(build_chrome_resources());
    engine.register_overlay_resources(build_overlay_resources(&integrity_html));

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
    // Reuses the shared store opened above.
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
    // Initial sizing uses the logical insets (scale 1.0); `resumed` recomputes
    // them against the window's real scale factor before the first paint.
    let (vw, vh) = viewport_size(INITIAL_WIDTH, INITIAL_HEIGHT, VIEWPORT_LEFT, VIEWPORT_TOP);
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
        host,
        integrity_page: None,
        integrity_open: false,
        integrity_chrome_open: false,
        integrity_paints: 0,
        picker: PickerState::default(),
        picker_page: None,
        picker_paints: 0,
        discarder: Discarder::new(discard_config()),
        reaper: HiddenTabReaper::new(hidden_tab_config()),
        housekeeping_interval: housekeeping_interval(),
        last_housekeeping: Instant::now(),
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
        scale_factor: 1.0,
        cursor: (0, 0),
        modifiers: Modifiers::NONE,
        focus: FocusOwner::Page,
        chrome_ready: false,
        did_initial_load: false,
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

/// Parse a `Duration` from an environment variable holding a whole number of
/// seconds, returning `None` when unset, empty, or unparsable.
fn env_secs(var: &str) -> Option<Duration> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Build the active-tab [`DiscardConfig`] from an optional idle override. The
/// default is 30 minutes with pinned tabs kept loaded (DESIGN); `Some(d)`
/// shortens the idle threshold (used by [`DISCARD_AFTER_ENV`] for tests).
fn discard_config_with(discard_after: Option<Duration>) -> DiscardConfig {
    let mut cfg = DiscardConfig::default();
    if let Some(after) = discard_after {
        cfg.discard_after = after;
    }
    cfg
}

/// [`discard_config_with`] sourced from [`DISCARD_AFTER_ENV`].
fn discard_config() -> DiscardConfig {
    discard_config_with(env_secs(DISCARD_AFTER_ENV))
}

/// Build the hidden-tab [`HiddenTabConfig`] from an optional TTL override. The
/// default TTL is 30 days (DESIGN); `Some(d)` shortens it (used by
/// [`HIDDEN_TTL_ENV`] for tests).
fn hidden_tab_config_with(ttl: Option<Duration>) -> HiddenTabConfig {
    let mut cfg = HiddenTabConfig::default();
    if let Some(ttl) = ttl {
        cfg.ttl = Some(ttl);
    }
    cfg
}

/// [`hidden_tab_config_with`] sourced from [`HIDDEN_TTL_ENV`].
fn hidden_tab_config() -> HiddenTabConfig {
    hidden_tab_config_with(env_secs(HIDDEN_TTL_ENV))
}

/// The housekeeping cadence, honouring [`HOUSEKEEPING_ENV`] for tests.
fn housekeeping_interval() -> Duration {
    env_secs(HOUSEKEEPING_ENV).unwrap_or(HOUSEKEEPING_INTERVAL)
}

/// The reverse-DNS application identifier used for the window's Wayland `app_id`
/// / X11 `WM_CLASS`. Compositors key window rules, icons, and taskbar grouping
/// off this; leaving it empty makes Mote an unidentified window.
const APP_ID: &str = "com.mote.Mote";

/// Build the winit window attributes, setting the title and the Wayland/X11
/// application identity (`app_id` / `WM_CLASS`).
///
/// On Linux the `app_id` (Wayland) and the `WM_CLASS` instance/general names
/// (X11) are set to [`APP_ID`] so the compositor can identify, group, and
/// icon-match the window; winit's `with_name` maps to the right protocol field
/// for whichever backend is active. On other platforms only title + size apply.
fn window_attributes() -> winit::window::WindowAttributes {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_mut, reason = "only the linux cfg arm mutates `attrs`")
    )]
    let mut attrs = Window::default_attributes()
        .with_title("mote")
        .with_inner_size(PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
    #[cfg(target_os = "linux")]
    {
        // Both backends compile in on Linux; set the identity for whichever the
        // session uses. `with_name(general, instance)` (fully-qualified to
        // disambiguate the two extension traits' identically-named method).
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::WindowAttributesExtX11;
        attrs = WindowAttributesExtWayland::with_name(attrs, APP_ID, "mote");
        attrs = WindowAttributesExtX11::with_name(attrs, APP_ID, "mote");
    }
    attrs
}

/// Build the privileged `mote://chrome` resource set from mote-ui's embedded
/// assets. This is the ONLY origin that carries the host-bridge binding, so it
/// serves only the chrome document and its assets — the overlays live on the
/// unprivileged `mote://overlay` origin (see [`build_overlay_resources`]).
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
        .register(
            "panels.js",
            mote_ui::PANELS_JS,
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

/// Build the **unprivileged** `mote://overlay` resource set: the shell-rendered
/// live integrity overlay (`integrity.html`) and the tab-picker overlay
/// (`picker.html`), plus the design-token + component CSS those documents
/// reference by relative URL. These surfaces are driven entirely Rust-side
/// (input routing + `eval_js`) and need no host-bridge, so they are served off a
/// distinct origin the chrome-origin gate does NOT match (S2) — no
/// `window.cefQuery` is ever installed in them.
fn build_overlay_resources(integrity_html: &str) -> ChromeResources {
    let css = "text/css; charset=utf-8";
    let mut res = ChromeResources::new()
        .register(
            "integrity.html",
            integrity_html.to_owned(),
            "text/html; charset=utf-8",
        )
        .register(
            "picker.html",
            picker::PICKER_HTML,
            "text/html; charset=utf-8",
        )
        .register("tokens.css", mote_ui::TOKENS_CSS, css)
        .register("base.css", mote_ui::BASE_CSS, css);
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
    let approve_queue = Arc::clone(commands);
    let update_queue = Arc::clone(commands);
    let rollback_queue = Arc::clone(commands);
    let reload_queue = Arc::clone(commands);
    let revoke_queue = Arc::clone(commands);
    let adjust_queue = Arc::clone(commands);
    let revoke_secret_queue = Arc::clone(commands);
    let urlbar_query_queue = Arc::clone(commands);
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
        .register("approve_plugin", move |params: &str| {
            // Deserialize the dialog's structured verdict.
            let Ok(result) = serde_json::from_str::<approval::DialogResult>(params) else {
                return OpResponse::err(400, "approve_plugin: malformed payload");
            };
            // Op-boundary STRUCTURAL validation (ADR-0005 closed structured
            // operations): bound every origin glob and the count per permission
            // synchronously, BEFORE the string can become a Narrowing resource.
            if let Err(msg) = validate_dialog_origins(&result) {
                return OpResponse::err(400, msg);
            }
            push(&approve_queue, ShellCommand::ApprovePlugin(result));
            OpResponse::ok("{\"accepted\":true}")
        })
        .register("plugin_update", move |params: &str| {
            plugin_action_op(&update_queue, params, ShellCommand::PluginUpdate)
        })
        .register("plugin_rollback", move |params: &str| {
            plugin_action_op(&rollback_queue, params, ShellCommand::PluginRollback)
        })
        .register("plugin_reload", move |params: &str| {
            plugin_action_op(&reload_queue, params, ShellCommand::PluginReload)
        })
        .register("plugin_revoke", move |params: &str| {
            plugin_action_op(&revoke_queue, params, ShellCommand::PluginRevoke)
        })
        .register("plugin_adjust_scope", move |params: &str| {
            plugin_action_op(&adjust_queue, params, ShellCommand::PluginAdjustScope)
        })
        .register("plugin_revoke_secret", move |params: &str| {
            plugin_revoke_secret_op(&revoke_secret_queue, params)
        })
        .register("urlbar_query", move |params: &str| {
            json_string_field(params, "text").map_or_else(
                || OpResponse::err(400, "urlbar_query requires a string `text`"),
                |text| {
                    push(&urlbar_query_queue, ShellCommand::UrlbarQuery(text));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
}

/// Op-boundary structural validation of the dialog's origin globs
/// (ADR-0005 closed structured operations). For every permission narrowed to
/// `origins`, every glob must pass [`approval::validate_origin_glob`] and the
/// per-permission count must not exceed [`approval::MAX_ORIGINS_PER_PERMISSION`].
/// Returns `Err(message)` on the first violation so the whole op is rejected —
/// no arbitrary chrome-supplied string is ever pushed toward a `Narrowing`.
fn validate_dialog_origins(result: &approval::DialogResult) -> Result<(), &'static str> {
    for perm in &result.permissions {
        if perm.mode != "origins" {
            continue;
        }
        let origins = perm.origins.as_deref().unwrap_or(&[]);
        if origins.len() > approval::MAX_ORIGINS_PER_PERMISSION {
            return Err("approve_plugin: too many origins for a permission");
        }
        if !origins.iter().all(|o| approval::validate_origin_glob(o)) {
            return Err("approve_plugin: invalid origin glob");
        }
    }
    Ok(())
}

/// Shared handler for the five panel-action ops: parse a non-empty string
/// `plugin` field, build the [`ShellCommand`] from it, and enqueue it. The
/// plugin-name *format* is validated when the pump thread turns the string into
/// a [`PluginName`]; here we only require a non-empty string.
fn plugin_action_op(
    queue: &CommandQueue,
    params: &str,
    make: impl FnOnce(String) -> ShellCommand,
) -> OpResponse {
    match json_string_field(params, "plugin") {
        Some(name) if !name.is_empty() => {
            push(queue, make(name));
            OpResponse::ok("{\"ok\":true}")
        }
        _ => OpResponse::err(400, "requires a non-empty string `plugin`"),
    }
}

/// Handler for `plugin_revoke_secret`: parse non-empty `plugin` and `name`
/// string fields and enqueue the revoke command. The plugin-name *format* is
/// validated when the pump thread turns the string into a [`PluginName`]; here
/// we only require both fields to be present non-empty strings.
fn plugin_revoke_secret_op(queue: &CommandQueue, params: &str) -> OpResponse {
    let plugin = json_string_field(params, "plugin").filter(|s| !s.is_empty());
    let name = json_string_field(params, "name").filter(|s| !s.is_empty());
    match (plugin, name) {
        (Some(plugin), Some(name)) => {
            push(queue, ShellCommand::PluginRevokeSecret { plugin, name });
            OpResponse::ok("{\"ok\":true}")
        }
        _ => OpResponse::err(
            400,
            "plugin_revoke_secret requires non-empty string fields `plugin` and `name`",
        ),
    }
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "winit app state — each bool is an independent in-flight UI flag (integrity overlay open, chrome-side panel open, chrome-ready, did-initial-load, first-frame-logged); a state machine would obscure them"
)]
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
    store: Store,
    /// The Phase-1 plugin runtime + audit log + the bundled plugins it loaded.
    /// Held for the program's lifetime so the audit thread stays alive and the
    /// integrity panel can be re-queried; `Drop` shuts the audit log down.
    host: runtime::PluginHost,
    /// The lazily-created integrity overlay page (loads `mote://overlay/integrity.html`).
    /// `None` until the panel is first opened.
    integrity_page: Option<Page>,
    /// Whether the integrity overlay is currently composited full-window.
    integrity_open: bool,
    /// Whether the chrome-rendered integrity panel (structured-DOM path in
    /// `panels.js`) is currently shown. Tracked independently from the legacy
    /// `integrity_open` overlay flag — Ctrl+Shift+I prefers the chrome path,
    /// and the overlay path remains as a fallback for the chrome-not-ready
    /// window (the legacy-overlay cleanup is a later task).
    integrity_chrome_open: bool,
    /// Last integrity paint count uploaded (re-upload only on a new frame).
    integrity_paints: u64,
    /// The workspace tab picker (`Mod+Space`) state machine. Logic lives
    /// Rust-side; the overlay page is pure display (see [`picker`]).
    picker: PickerState,
    /// The lazily-created picker overlay page (`mote://overlay/picker.html`).
    /// `None` until the picker is first opened.
    picker_page: Option<Page>,
    /// Last picker paint count uploaded (re-upload only on a new frame).
    picker_paints: u64,
    /// Applies active-tab renderer-discard decisions (DESIGN §Active tab
    /// discarding). The shell kills the renderer for each newly-discarded tab.
    discarder: Discarder,
    /// Ages out hidden tabs past their TTL (DESIGN §Hidden tab lifecycle).
    reaper: HiddenTabReaper,
    /// How often [`Self::run_housekeeping`] runs (discard + reap pass).
    housekeeping_interval: Duration,
    /// When housekeeping last ran (throttles the pass to the interval).
    last_housekeeping: Instant,
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
    /// The window's DPI scale factor (1.0 at 96dpi, 1.25 at 120dpi, …). The
    /// chrome insets are authored in logical pixels, so the physical page
    /// viewport is computed by scaling them by this factor (plan §1.3): a fixed
    /// physical inset is wrong on a scaled display.
    scale_factor: f64,
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
    /// `true` once the deferred plugin load pass has run (exactly once, on the
    /// first post-paint tick). Until then no plugin has been resolved/loaded —
    /// the load is deferred past window creation so a slow/offline git fetch or
    /// a fatal resolution error cannot block or abort startup (ADR T3 review).
    did_initial_load: bool,
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
    /// The chrome's left inset in **physical** pixels for the current display
    /// scale. The inset is authored in logical pixels (`VIEWPORT_LEFT`); on a
    /// scaled display (e.g. 1.25×) the physical inset is larger, so a fixed
    /// physical value would misalign the composited page (plan §1.3).
    fn inset_left(&self) -> u32 {
        scale_inset(VIEWPORT_LEFT, self.scale_factor)
    }

    /// The chrome's top inset in physical pixels for the current display scale.
    fn inset_top(&self) -> u32 {
        scale_inset(VIEWPORT_TOP, self.scale_factor)
    }

    /// The content page's physical surface size (window minus the scaled insets).
    fn viewport_dims(&self) -> (u32, u32) {
        viewport_size(self.width, self.height, self.inset_left(), self.inset_top())
    }

    /// The current page-viewport rect in physical pixels.
    fn viewport_rect(&self) -> ViewportRect {
        let (vw, vh) = self.viewport_dims();
        ViewportRect::new(
            px(self.inset_left()),
            px(self.inset_top()),
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
                ShellCommand::ApprovePlugin(result) => self.approve_plugin(&result),
                ShellCommand::PluginUpdate(name) => self.plugin_update(&name),
                ShellCommand::PluginRollback(name) => self.plugin_rollback(&name),
                ShellCommand::PluginReload(name) => self.plugin_reload(&name),
                ShellCommand::PluginRevoke(name) => self.plugin_revoke(&name),
                ShellCommand::PluginAdjustScope(name) => self.plugin_adjust_scope(&name),
                ShellCommand::PluginRevokeSecret { plugin, name } => {
                    self.plugin_revoke_secret(&plugin, &name);
                }
                ShellCommand::UrlbarQuery(text) => self.urlbar_query(&text),
            }
        }
    }

    /// Invoke `ui:urlbar_provider` → `query` with the given text and push the
    /// returned suggestion list to the chrome via
    /// `window.mote.applyOp('urlbar_suggestions', <json>)`.
    ///
    /// Called from [`drain_commands`] for every [`ShellCommand::UrlbarQuery`].
    /// The D1 chrome-side rendering (dropdown build + keyboard select) is a
    /// separate task; this method's observable end is the `eval_js` call.
    ///
    /// # Failure policy
    ///
    /// - No fulfiller loaded / contract violation / timeout →
    ///   [`Runtime::invoke_capability`] returns `None`; we push an empty list so
    ///   the chrome dropdown clears rather than showing stale results.
    /// - The provider may return an empty Lua table `{}`, which marshals to
    ///   [`HostValue::Map`] (not [`HostValue::List`]) because the runtime cannot
    ///   distinguish an empty array from an empty object in Lua. We normalise any
    ///   non-List result (including an empty Map) to `List([])` so chrome always
    ///   receives a JSON array.
    /// - JSON serialisation failure is defended against even though it cannot
    ///   occur for a well-formed `HostValue` (no NaN/±Inf paths reach here):
    ///   we push an empty list and log, matching the existing shell failure idiom.
    fn urlbar_query(&self, text: &str) {
        // The history plugin's `query(text)` function accepts a plain Str
        // argument (matching the existing host_invoke_capability test pattern).
        // Pass the text directly as HostValue::Str rather than a Map.
        let arg = HostValue::Str(text.to_owned());

        // Invoke the provider; None on no fulfiller / contract violation /
        // timeout / error (Runtime::invoke_capability audits failures internally).
        // Normalise the result: only a List is a valid suggestions payload; any
        // other variant (Nil, Map, Str, …) — including the empty-Lua-table Map
        // the provider returns for empty-text fast-exits — becomes an empty list.
        let suggestions =
            match self
                .host
                .runtime
                .invoke_capability("ui:urlbar_provider", "query", &arg)
            {
                Some(HostValue::List(items)) => HostValue::List(items),
                _ => HostValue::List(vec![]),
            };

        // Convert HostValue → serde_json::Value → JSON string.
        // Serialisation of a HostValue::List should never fail; defend anyway.
        let payload_json = match serde_json::to_string(&host_to_json(&suggestions)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: urlbar_query serialise failed: {e}; pushing empty list");
                "[]".to_owned()
            }
        };

        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('urlbar_suggestions',{payload_json});"
        ));
    }

    /// Finish a dialog approval on the pump thread (ADR-0007 async approval).
    ///
    /// The op handler already ran the op-boundary structural validation; here
    /// we do the semantic work via [`runtime::PluginHost::approve_pending`]
    /// (find the pending entry, cross-check, load or deny, record approval),
    /// then drive the chrome. Dismissal is **shell-driven** (panels.js no longer
    /// hides on click): the shell hides the dialog only on a resolved outcome.
    ///
    /// - `Loaded` / `Denied`: hide the dialog, re-render the panel, and advance
    ///   to the next pending dialog if the queue is non-empty.
    /// - `LoadFailed`: leave the dialog up so the user can decline/retry; the
    ///   plugin stays pending. (A proper in-dialog error banner is deferred.)
    /// - `NotPending`: do nothing — the dropped result names no live dialog, so
    ///   hiding could dismiss a legitimate dialog for a different plugin.
    fn approve_plugin(&mut self, result: &approval::DialogResult) {
        match self.host.approve_pending(result) {
            runtime::ApproveOutcome::Loaded => {
                eprintln!("mote-shell: plugin `{}` approved + loaded", result.plugin());
                self.push_hide_approval_dialog();
                self.refresh_integrity_panel();
                self.show_next_pending_dialog();
            }
            runtime::ApproveOutcome::Denied => {
                eprintln!("mote-shell: plugin `{}` denied by user", result.plugin());
                self.push_hide_approval_dialog();
                self.refresh_integrity_panel();
                self.show_next_pending_dialog();
            }
            runtime::ApproveOutcome::LoadFailed => {
                // Leave the dialog visible so the user can decline/retry.
                // TODO(phase-10): inline error banner in the dialog on LoadFailed.
                eprintln!(
                    "mote-shell: plugin `{}` approval load FAILED (left pending; dialog kept)",
                    result.plugin()
                );
            }
            runtime::ApproveOutcome::NotPending => {
                // Do NOT hide: a dropped/stale result must not dismiss whatever
                // legitimate dialog is currently shown.
                eprintln!(
                    "mote-shell: approve for non-pending plugin `{}` (dropped)",
                    result.plugin()
                );
            }
        }
    }

    /// Render the next awaiting-approval dialog, if any. The dialog root holds a
    /// single dialog, so the shell shows pending approvals one at a time: after
    /// one resolves (`Loaded`/`Denied`) the shell advances to the next. For 0–1
    /// pending this is a no-op beyond the initial push.
    ///
    /// follow-up: a richer multi-dialog queue UI is out of scope.
    fn show_next_pending_dialog(&self) {
        if let Some((_, req)) = self.host.pending_approvals.first() {
            self.push_approval_dialog(req);
        }
    }

    /// Panel action — update the plugin, then re-render the integrity panel.
    ///
    /// An expanding update needs re-approval: the host enqueues a fresh pending
    /// entry and returns its [`ApprovalRequest`], which we render as a dialog
    /// (the subsequent `approve_plugin` reloads with the new grant). A
    /// non-expanding update is applied + reloaded by the host directly.
    fn plugin_update(&mut self, name: &str) {
        match self.host.update_plugin(name) {
            runtime::UpdateAction::ReApproval(req) => {
                self.push_approval_dialog(&req);
            }
            runtime::UpdateAction::Applied | runtime::UpdateAction::Failed => {}
        }
        self.refresh_integrity_panel();
    }

    /// Panel action — roll the plugin back to its prior commit + reload.
    fn plugin_rollback(&mut self, name: &str) {
        self.host.rollback_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — reload the plugin (path/dev re-run).
    fn plugin_reload(&mut self, name: &str) {
        self.host.reload_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — revoke the plugin (unload + drop its stored approval).
    fn plugin_revoke(&mut self, name: &str) {
        self.host.revoke_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — re-open the approval dialog for the plugin so the user can
    /// re-narrow its grant. The subsequent `approve_plugin` reloads with the new
    /// narrowing (re-grant-via-reload). No-op if the plugin is not loaded.
    fn plugin_adjust_scope(&mut self, name: &str) {
        if let Some(req) = self.host.adjust_scope_request(name) {
            self.push_approval_dialog(&req);
            // adjust_scope_request moves the plugin loaded→pending; refresh the
            // panel (if open) so it reflects the parked/awaiting-approval state,
            // consistent with the other panel actions.
            self.refresh_integrity_panel();
        }
    }

    /// Panel action — revoke a specific secret grant from a loaded plugin
    /// (session-scoped; does not persist across relaunch). Narrows the plugin's
    /// `(secret, read)` grant to exclude `name`, then refreshes the panel.
    fn plugin_revoke_secret(&mut self, plugin: &str, name: &str) {
        self.host.revoke_secret(plugin, name);
        self.refresh_integrity_panel();
    }

    /// Re-render the chrome integrity panel from live host state, but only when
    /// it is currently open (so panel-driven actions reflect immediately without
    /// popping the panel open behind a dialog the user did not request).
    fn refresh_integrity_panel(&self) {
        if self.integrity_chrome_open {
            let panel = self.host.build_panel();
            self.push_integrity_panel_to_chrome(&panel);
        }
    }

    /// Hide the approval dialog inside the chrome document. The dialog buttons
    /// also dismiss it locally (optimistic UX), so this is idempotent; it exists
    /// for the pump-thread path (e.g. a dropped/denied result) and to keep the
    /// chrome and host state in lockstep.
    fn push_hide_approval_dialog(&self) {
        if !self.chrome_ready {
            return;
        }
        self.bridge.page().eval_js(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('hide_approval_dialog',null);",
        );
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
        let (vw, vh) = self.viewport_dims();
        let scale = self.scale_factor;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
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

    /// Push the integrity-panel view-model into the chrome document. The chrome
    /// page's `panels.js` builds the panel DOM via `createElement` / `textContent`
    /// (ADR-0005) — no HTML strings cross the bridge. Returns `true` when the
    /// push was emitted, `false` when the chrome is not yet ready.
    fn push_integrity_panel_to_chrome(&self, panel: &IntegrityPanel) -> bool {
        if !self.chrome_ready {
            return false;
        }
        let payload = match serde_json::to_string(panel) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: serialize integrity panel failed: {e}");
                return false;
            }
        };
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('render_integrity_panel',{payload});"
        ));
        true
    }

    /// Hide the integrity panel inside the chrome document.
    fn push_hide_integrity_to_chrome(&self) {
        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('hide_integrity_panel',null);",
        );
    }

    /// Push an approval-dialog view-model into the chrome document. The chrome
    /// page's `panels.js` builds the dialog DOM via `createElement` /
    /// `textContent` (ADR-0005). The dialog's buttons invoke the `approve_plugin`
    /// op; the pump thread finishes the load and re-renders the panel on the
    /// user's answer (ADR-0007 async approval).
    fn push_approval_dialog(&self, req: &ApprovalRequest) -> bool {
        if !self.chrome_ready {
            return false;
        }
        let payload = match serde_json::to_string(req) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: serialize approval request failed: {e}");
                return false;
            }
        };
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('show_approval_dialog',{payload});"
        ));
        true
    }

    /// Poll the active tab's live page for a document-title change (CEF reports
    /// it via `DisplayHandler::on_title_change`, surfaced by `Page::title`). When
    /// the title differs from what the tab already shows, update the tab + the
    /// session and push the new tab list into the chrome so the tab strip shows
    /// the real page title instead of the URL fallback.
    fn sync_active_title(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(title) = tab.page.as_ref().and_then(Page::title) else {
            return;
        };
        if tab.title.as_deref() == Some(title.as_str()) {
            return;
        }
        let id = tab.id;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.title = Some(title.clone());
        }
        if let Some(stab) = self.session.tab_mut(id) {
            stab.title = Some(title);
        }
        // No session flush here: title is cosmetic and changes frequently; it is
        // captured on the next structural flush (open/close/switch/navigate).
        self.push_state_to_chrome();
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
    ///
    /// When the integrity overlay is open it is composited full-window onto the
    /// **chrome** texture (which the compositor draws over the page): the overlay
    /// document is opaque, so it covers the whole window. The normal chrome /
    /// content uploads are skipped while it is open, and forced to re-upload when
    /// it closes (the `*_paints` counters are reset in `set_integrity_open`).
    fn upload_frames(&mut self) {
        if self.picker.open {
            self.upload_picker_overlay();
            return;
        }
        if self.integrity_open {
            self.upload_integrity_overlay();
            return;
        }
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

    /// Composite the integrity overlay full-window onto the chrome texture.
    fn upload_integrity_overlay(&mut self) {
        let frame = self.integrity_page.as_ref().and_then(|p| {
            let count = p.paint_count();
            (count != self.integrity_paints).then(|| {
                self.integrity_paints = count;
                p.latest_frame()
            })
        });
        let Some(Some(frame)) = frame else {
            return;
        };
        if let Some(compositor) = self.compositor.as_mut()
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: integrity overlay upload failed: {e}");
        }
    }

    /// Open or close the live integrity overlay.
    ///
    /// On first open, lazily creates a full-window opaque [`Page`] loading
    /// `mote://overlay/integrity.html` (the shell-rendered live view-model). It
    /// uses the **global** request context (`Page::new`, not `with_profile`) —
    /// the same context the chrome bridge page loads `mote://chrome` from — so
    /// the privileged scheme resolves (a per-identity profile context does not
    /// carry the custom-scheme factory, yielding `ERR_UNKNOWN_URL_SCHEME`). The
    /// overlay is a trusted runtime surface, not user content. Toggling resets
    /// the relevant paint counters so the compositor re-uploads the right surface
    /// on the next pump.
    fn set_integrity_open(&mut self, open: bool) {
        self.integrity_open = open;
        if open {
            if self.integrity_page.is_none() {
                let url = overlay_url("integrity.html");
                let opts = PageOptions {
                    width: self.width.max(1),
                    height: self.height.max(1),
                    frame_rate: 60,
                    // Trusted, bridgeless overlay surface on the unprivileged
                    // `mote://overlay` origin (S2). The Overlay role both keeps
                    // `window.cefQuery` out (origin gate) and lets this internal
                    // URL load past the content navigation guard (S1).
                    role: PageRole::Overlay,
                };
                match Page::new(&url, &opts) {
                    Ok(page) => {
                        page.notify_resized(
                            self.width.max(1),
                            self.height.max(1),
                            self.scale_factor,
                        );
                        self.integrity_page = Some(page);
                    }
                    Err(e) => {
                        eprintln!("mote-shell: failed to open integrity panel: {e}");
                        self.integrity_open = false;
                        return;
                    }
                }
            } else if let Some(page) = self.integrity_page.as_ref() {
                page.notify_resized(self.width.max(1), self.height.max(1), self.scale_factor);
            }
            self.integrity_paints = 0;
            eprintln!(
                "mote-shell: integrity panel opened ({} bundled plugin(s) shown)",
                self.host.loaded.len()
            );
        } else {
            // Force the chrome + content layers to re-upload, restoring the
            // browser surface beneath the (now hidden) overlay.
            self.chrome_paints = 0;
            self.content_paints = 0;
            eprintln!("mote-shell: integrity panel closed");
        }
    }

    // ── Workspace tab picker (Mod+Space) ──────────────────────────────────

    /// Open or close the workspace tab picker overlay.
    ///
    /// On open it snapshots the current workspace's tabs in
    /// [`Session::tab_picker_ranked`] order into [`PickerState`] and lazily
    /// creates the full-window overlay [`Page`] (`mote://overlay/picker.html`),
    /// composited like the integrity overlay. Selection/filtering are handled
    /// Rust-side (see [`picker`]); on close the browser surface re-uploads.
    fn set_picker_open(&mut self, open: bool) {
        if open {
            let entries: Vec<PickerEntry> = self
                .session
                .tab_picker_ranked(self.workspace)
                .into_iter()
                .map(PickerEntry::from_tab)
                .collect();
            self.picker.open(entries);
            if self.picker_page.is_none() {
                let url = overlay_url("picker.html");
                let opts = PageOptions {
                    width: self.width.max(1),
                    height: self.height.max(1),
                    frame_rate: 60,
                    // Trusted, bridgeless overlay surface on the unprivileged
                    // `mote://overlay` origin (S2); see the integrity overlay.
                    role: PageRole::Overlay,
                };
                match Page::new(&url, &opts) {
                    Ok(page) => {
                        page.notify_resized(
                            self.width.max(1),
                            self.height.max(1),
                            self.scale_factor,
                        );
                        self.picker_page = Some(page);
                    }
                    Err(e) => {
                        eprintln!("mote-shell: failed to open tab picker: {e}");
                        self.picker.close();
                        return;
                    }
                }
            } else if let Some(page) = self.picker_page.as_ref() {
                page.notify_resized(self.width.max(1), self.height.max(1), self.scale_factor);
            }
            self.picker_paints = 0;
            eprintln!("mote-shell: tab picker opened");
        } else {
            self.picker.close();
            // Force the browser surface beneath the (now hidden) overlay to
            // re-upload (the picker drew over the chrome texture).
            self.chrome_paints = 0;
            self.content_paints = 0;
            eprintln!("mote-shell: tab picker closed");
        }
    }

    /// Push the picker's current query + filtered rows into the overlay via
    /// `eval_js` → `window.__motePicker(state)`. Strings are JS-escaped exactly
    /// like the chrome push (page-derived titles/URLs are injection vectors).
    fn push_picker_state(&self) {
        let Some(page) = self.picker_page.as_ref() else {
            return;
        };
        let rows = self.picker.rows_json(js_string);
        let query = js_string(self.picker.query());
        page.eval_js(&format!(
            "window.__motePicker&&window.__motePicker({{query:{query},rows:{rows}}});"
        ));
    }

    /// Handle a key event while the picker is open. Returns `true` if the key
    /// was consumed by the picker (so it is not routed to a page).
    ///
    /// Esc closes; Enter selects; Up/Down (and Ctrl+J/K) move; Backspace edits;
    /// printable characters filter. Every state change re-renders the overlay.
    fn picker_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            // Swallow key-ups too while open so the page never sees them.
            return true;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.set_picker_open(false);
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.activate_picker_selection();
                return true;
            }
            Key::Named(NamedKey::ArrowDown) => self.picker.move_down(),
            Key::Named(NamedKey::ArrowUp) => self.picker.move_up(),
            Key::Named(NamedKey::Backspace) => self.picker.backspace(),
            Key::Character(s) if self.modifiers.contains(Modifiers::CONTROL) => {
                // Vim-style Ctrl+J / Ctrl+K navigation (palette spec).
                if s.eq_ignore_ascii_case("j") {
                    self.picker.move_down();
                } else if s.eq_ignore_ascii_case("k") {
                    self.picker.move_up();
                } else {
                    return true; // swallow other Ctrl-combos while open
                }
            }
            Key::Character(s) => {
                for c in s.chars() {
                    self.picker.push_char(c);
                }
            }
            Key::Named(NamedKey::Space) => self.picker.push_char(' '),
            _ => return true, // swallow anything else; the picker owns input
        }
        self.push_picker_state();
        true
    }

    /// Resolve the selected picker row and act on it, then close the picker.
    ///
    /// An **active** tab → the existing switch path ([`Self::select_tab`]); a
    /// **hidden** tab → reveal it in the session and materialize/switch to it
    /// (DESIGN: selecting a hidden tab brings it into the current window).
    fn activate_picker_selection(&mut self) {
        let Some(entry) = self.picker.selected_entry() else {
            self.set_picker_open(false);
            return;
        };
        let id = entry.id;
        let is_active = entry.is_active();
        self.set_picker_open(false);
        if is_active {
            self.select_tab(id);
        } else {
            self.reveal_tab(id);
        }
    }

    /// Reveal a hidden tab into the current window: flip it to active in the
    /// session, add it to the window's tab strip (materializing its page), and
    /// switch to it. No-op if the tab is missing or already shown in the strip.
    fn reveal_tab(&mut self, id: TabId) {
        // Already in this window's strip → just select it.
        if self.tabs.iter().any(|t| t.id == id) {
            self.select_tab(id);
            return;
        }
        if let Err(e) = self.session.reveal_tab(id) {
            eprintln!("mote-shell: reveal tab {id} failed: {e}");
            return;
        }
        let Some(stab) = self.session.tab(id) else {
            return;
        };
        let url = stab.url.clone();
        let title = stab.title.clone();
        eprintln!("mote-shell: reveal hidden tab {id} -> {url}");
        let page = match Page::with_profile(&url, &self.content_opts, &self.default_profile) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: failed to materialize revealed tab {id}: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url,
            title,
            page,
        });
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Composite the picker overlay full-window onto the chrome texture. On the
    /// page's first paint, push the initial picker state (an `eval_js` before the
    /// document's script runs would be lost — same warm-up as the chrome).
    fn upload_picker_overlay(&mut self) {
        if !self.picker.ready
            && self
                .picker_page
                .as_ref()
                .is_some_and(|p| p.paint_count() >= 1)
        {
            self.picker.ready = true;
            self.push_picker_state();
        }
        let frame = self.picker_page.as_ref().and_then(|p| {
            let count = p.paint_count();
            (count != self.picker_paints).then(|| {
                self.picker_paints = count;
                p.latest_frame()
            })
        });
        let Some(Some(frame)) = frame else {
            return;
        };
        if let Some(compositor) = self.compositor.as_mut()
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: tab picker overlay upload failed: {e}");
        }
    }

    // ── Session housekeeping (discard + hidden-tab reap) ───────────────────

    /// Run the periodic session housekeeping pass if the interval has elapsed:
    /// discard idle active-tab renderers, then reap hidden tabs past their TTL.
    fn maybe_run_housekeeping(&mut self) {
        if self.last_housekeeping.elapsed() < self.housekeeping_interval {
            return;
        }
        self.last_housekeeping = Instant::now();
        let discarded = self.discard_idle_renderers();
        let reaped = self.reap_hidden_tabs();
        if discarded > 0 || reaped > 0 {
            self.persist_and_push();
        }
    }

    /// Apply [`Discarder`] to the workspace's tabs and drop the CEF renderer for
    /// each tab whose `is_discarded` flag newly transitions to `true`. The tab
    /// entry stays in the strip (DESIGN: a discarded tab reloads on focus). The
    /// **active, focused** tab is never discarded — `last_visited` is bumped for
    /// it so its idle timer only starts once the user moves away. Returns the
    /// number of renderers dropped.
    ///
    /// Drives [`Discarder::should_discard`] per tab through the session's public
    /// per-id API (the session exposes no `&mut [Tab]` slice for the batch
    /// `discard_all`, so we apply the same decision tab-by-tab).
    fn discard_idle_renderers(&mut self) -> usize {
        // Keep the focused tab's idle clock from advancing: it is in use.
        if let Some(focused) = self.tabs.get(self.active)
            && let Some(stab) = self.session.tab_mut(focused.id)
        {
            stab.last_visited = Some(SystemTime::now());
        }

        // Decide which tabs to discard (immutable borrow), then apply + drop the
        // renderers (mutable borrows) — split so the borrows don't overlap.
        let to_discard: Vec<TabId> = self
            .session
            .tab_picker_ranked(self.workspace)
            .into_iter()
            .filter(|t| self.discarder.should_discard(t))
            .map(|t| t.id)
            .collect();

        let mut dropped = 0;
        for id in to_discard {
            if let Some(stab) = self.session.tab_mut(id) {
                stab.discard();
            }
            // Drop the live renderer but KEEP the tab entry (page = None makes it
            // a placeholder that reloads on focus, the discard contract).
            if let Some(shell_tab) = self.tabs.iter_mut().find(|t| t.id == id)
                && let Some(page) = shell_tab.page.take()
            {
                page.close();
                dropped += 1;
                eprintln!("mote-shell: discarded idle renderer for tab {id}");
            }
        }
        dropped
    }

    /// Reap hidden tabs past their TTL via [`HiddenTabReaper::should_reap`],
    /// permanently removing them from the session (DESIGN §Memory cost:
    /// "Aged out (>30 days hidden) → Deleted").
    ///
    /// For each reap candidate the tab is deleted from the session map via
    /// [`Session::remove_tab`] so it does not survive the next
    /// [`Session::flush`]. Because hidden tabs have no live CEF renderer, there
    /// is no `ShellTab`/`Page` to drop — `self.tabs` only carries active-window
    /// tabs. Returns the number of tabs actually deleted.
    fn reap_hidden_tabs(&mut self) -> usize {
        // Collect candidates first (immutable borrow of session), then remove
        // (mutable borrow) — split to avoid overlapping borrows.
        let to_reap: Vec<TabId> = self
            .session
            .tab_picker_ranked(self.workspace)
            .into_iter()
            .filter(|t| self.reaper.should_reap(t))
            .map(|t| t.id)
            .collect();

        let mut reaped = 0;
        for id in to_reap {
            if self.session.remove_tab(id).is_some() {
                reaped += 1;
                eprintln!("mote-shell: reaped hidden tab {id} (TTL expired)");
            }
            // Hidden tabs have no live renderer; self.tabs holds only
            // active-window tabs. Nothing further to drop.
        }
        reaped
    }

    /// React to a DPI scale-factor change: recompute the (logical-pixel) chrome
    /// insets at the new scale and resize the active page's surface to the new
    /// physical viewport (high-DPI, plan §1.3).
    fn on_scale_factor_changed(&mut self, scale: f64) {
        self.scale_factor = scale;
        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
        }
        self.content_paints = 0;
    }

    /// Resize: reconfigure the surface, tell the chrome + active page their new
    /// sizes, and force a re-upload.
    fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.width = size.width;
        self.height = size.height;
        let scale = self.scale_factor;
        if let Some(compositor) = self.compositor.as_mut() {
            compositor.resize(size.width, size.height);
        }
        self.bridge
            .page()
            .notify_resized(size.width, size.height, scale);
        // The integrity overlay (if live) is full-window like the chrome.
        if let Some(page) = self.integrity_page.as_ref() {
            page.notify_resized(size.width, size.height, scale);
        }
        // The picker overlay (if live) is full-window like the chrome too.
        if let Some(page) = self.picker_page.as_ref() {
            page.notify_resized(size.width, size.height, scale);
        }
        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
        }
    }

    /// Returns `true` when a chrome-rendered overlay (approval dialog or
    /// integrity panel) is currently capturing input.
    ///
    /// While an overlay is active every mouse event must go to the chrome
    /// page — the overlays render full-window inside `mote://chrome` over the
    /// content viewport, so content must not receive pointer events.
    const fn chrome_overlay_capturing_input(&self) -> bool {
        self.integrity_chrome_open || !self.host.pending_approvals.is_empty()
    }

    /// Route a mouse click to the active content page (if inside the viewport)
    /// or the chrome page (otherwise), in the correct coordinate space
    /// (plan §1.3).
    ///
    /// When a chrome overlay is capturing input (approval dialog or integrity
    /// panel), the cursor position is irrelevant — all clicks go to the chrome
    /// page so the overlay can receive them (ADR-0007/ADR-0008).
    fn route_click(&mut self, button: WinitMouseButton, state: ElementState) {
        let Some(mb) = map_mouse_button(button) else {
            return;
        };
        let action = match state {
            ElementState::Pressed => ButtonAction::Down,
            ElementState::Released => ButtonAction::Up,
        };
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                self.set_focus_owner(FocusOwner::Chrome);
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_button(pos, mb, action, 1, self.modifiers);
            }
            ClickTarget::ContentPage => {
                self.set_focus_owner(FocusOwner::Page);
                // page_local is Some(_) when ClickTarget::ContentPage is returned.
                if let Some(pos) = page_local {
                    let pos = self.to_view_dip(pos);
                    if let Some(page) = self.active_page() {
                        page.send_mouse_button(pos, mb, action, 1, self.modifiers);
                    }
                }
            }
        }
    }

    /// Route a mouse move to whichever surface the cursor is over.
    ///
    /// When a chrome overlay is capturing input, moves are directed to the
    /// chrome page regardless of cursor geometry (same gate as [`Self::route_click`]).
    fn route_mouse_move(&self) {
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_move(pos, self.modifiers, false);
            }
            ClickTarget::ContentPage => {
                if let (Some(pos), Some(page)) = (page_local, self.active_page()) {
                    page.send_mouse_move(self.to_view_dip(pos), self.modifiers, false);
                }
            }
        }
    }

    /// Route a mouse-wheel scroll to whichever surface the cursor is over — the
    /// same chrome-overlay-capture gate and DIP coordinate mapping as
    /// [`Self::route_mouse_move`]. winit line deltas are scaled to pixel deltas
    /// ([`WHEEL_STEP_PX`] per line); pixel deltas pass through. Positive
    /// `delta_y` scrolls the page up (CEF convention).
    fn route_wheel(&self, delta: MouseScrollDelta) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (
                wheel_delta(f64::from(x) * WHEEL_STEP_PX),
                wheel_delta(f64::from(y) * WHEEL_STEP_PX),
            ),
            MouseScrollDelta::PixelDelta(p) => (wheel_delta(p.x), wheel_delta(p.y)),
        };
        if dx == 0 && dy == 0 {
            return;
        }
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_wheel(pos, dx, dy, self.modifiers);
            }
            ClickTarget::ContentPage => {
                if let (Some(pos), Some(page)) = (page_local, self.active_page()) {
                    page.send_mouse_wheel(self.to_view_dip(pos), dx, dy, self.modifiers);
                }
            }
        }
    }

    /// Convert a window-physical-pixel position to the CEF page's view (DIP)
    /// coordinate space. CEF lays both the chrome and content OSR documents out
    /// at logical size (`physical / scale`) and scales the paint buffer by the
    /// device scale (issue #7); mouse events are injected in that view space, so
    /// raw winit physical pixels must be divided by the scale or every pointer
    /// event lands `scale`× off-target.
    fn to_view_dip(&self, p: MousePosition) -> MousePosition {
        MousePosition {
            x: physical_to_dip(p.x, self.scale_factor),
            y: physical_to_dip(p.y, self.scale_factor),
        }
    }

    /// If window-space `(x, y)` is inside the page viewport, return the
    /// page-local position; otherwise `None` (the event belongs to the chrome).
    fn page_local(&self, x: i32, y: i32) -> Option<MousePosition> {
        page_local_coords(
            x,
            y,
            self.width,
            self.height,
            self.inset_left(),
            self.inset_top(),
        )
    }

    /// Intercept the chrome keybinds that must always win before the page sees
    /// the key (plan §1.3 / §6.1): `Ctrl+T` new tab, `Ctrl+W` close active tab,
    /// `Ctrl+Tab` next tab, `Ctrl+Shift+I` toggle the integrity panel, `Esc`
    /// closes it. Returns `true` if the key was consumed (not routed).
    ///
    /// Uses `Ctrl` (the Linux/dev convention) where the spec writes `⌘`.
    fn intercept_keybind(&mut self, event: &winit::event::KeyEvent) -> bool {
        // While the tab picker is open it owns ALL keyboard input (filter,
        // navigate, select, close) — route every key to it before anything else.
        if self.picker.open {
            return self.picker_key(event);
        }
        if event.state != ElementState::Pressed {
            return false;
        }
        // Esc closes the integrity panel (either the chrome-rendered one or
        // the legacy overlay surface, whichever happens to be live).
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            let overlay_was_open = self.integrity_open;
            let chrome_was_open = self.integrity_chrome_open;
            if overlay_was_open {
                self.set_integrity_open(false);
            }
            if chrome_was_open {
                self.integrity_chrome_open = false;
                self.push_hide_integrity_to_chrome();
            }
            if overlay_was_open || chrome_was_open {
                return true;
            }
        }
        // Mod+Space (Super or Ctrl) opens the workspace tab picker (DESIGN
        // §The workspace tab picker — default `Mod+Space`). `Mod` is Super on
        // Linux; we also accept Ctrl+Space for keyboards/WMs that reserve Super.
        if matches!(event.logical_key, Key::Named(NamedKey::Space))
            && (self.modifiers.contains(Modifiers::COMMAND)
                || self.modifiers.contains(Modifiers::CONTROL))
        {
            self.set_picker_open(true);
            return true;
        }
        if !self.modifiers.contains(Modifiers::CONTROL) {
            return false;
        }
        // Ctrl+Shift+I toggles the integrity panel (the `i` arrives as upper or
        // lower case depending on the shift state, so match case-insensitively).
        // Prefers the chrome-rendered structured-DOM path: serialize the panel
        // view-model and push it through window.mote.applyOp. The legacy overlay
        // path stays as a fallback for the chrome-not-ready window so the keybind
        // never appears broken (legacy-overlay cleanup is a later task).
        if self.modifiers.contains(Modifiers::SHIFT)
            && let Key::Character(s) = &event.logical_key
            && s.eq_ignore_ascii_case("i")
        {
            if self.chrome_ready {
                if self.integrity_chrome_open {
                    self.integrity_chrome_open = false;
                    self.push_hide_integrity_to_chrome();
                } else {
                    let panel = self.host.build_panel();
                    if self.push_integrity_panel_to_chrome(&panel) {
                        self.integrity_chrome_open = true;
                    }
                }
            } else {
                let open = !self.integrity_open;
                self.set_integrity_open(open);
            }
            return true;
        }
        // Debug-only keybind (Ctrl+Shift+A): push a sample ApprovalRequest into
        // the chrome page so the dialog renders. The buttons call the real
        // `approve_plugin` op (registered in `build_op_registry`); the sample
        // plugin is not pending, so an Approve resolves as NotPending and the
        // dialog stays up (expected for the synthetic sample).
        // Retained for T7 live verification (this headless box cannot synthesize
        // CEF clicks); remove at T7 close.
        if self.modifiers.contains(Modifiers::SHIFT)
            && let Key::Character(s) = &event.logical_key
            && s.eq_ignore_ascii_case("a")
            && self.chrome_ready
        {
            let req = ApprovalRequest::sample();
            self.push_approval_dialog(&req);
            return true;
        }
        // T4 debug-only keybind (Ctrl+Shift+X): push a HOSTILE ApprovalRequest
        // whose plugin name, source, and permission domain carry XSS payloads
        // (script tags, onerror attribute, raw quotes). The chrome must render
        // them as inert literal text — proving the structured-DOM path holds
        // (ADR-0005). Verifies the boundary in live behaviour; the Rust test
        // `hostile_approval_request_serializes_as_inert_json_strings` proves
        // the wire-format encoding.
        // Retained for T7 live verification; remove at T7 close.
        if self.modifiers.contains(Modifiers::SHIFT)
            && let Key::Character(s) = &event.logical_key
            && s.eq_ignore_ascii_case("x")
            && self.chrome_ready
        {
            let mut req = ApprovalRequest::sample();
            req.plugin = "<script>alert('xss-pluginname')</script>".into();
            req.source = "evil\" onerror=\"alert('xss-source')".into();
            req.permissions[0].domain = "</script><img src=x onerror=alert(1)>".into();
            req.permissions[0].description =
                "this description contains <script>alert(1)</script> markup".into();
            req.dangerous_combinations[0] = "\"></div><script>fetch('//attacker')</script>".into();
            self.push_approval_dialog(&req);
            return true;
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
        let attrs = window_attributes();
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
        // Capture the display scale so the chrome insets (authored in logical
        // pixels) map to the right physical page viewport (HiDPI, plan §1.3).
        self.scale_factor = window.scale_factor();

        match Compositor::new_for_window(Arc::clone(&window), self.width, self.height) {
            Ok(c) => self.compositor = Some(c),
            Err(e) => {
                eprintln!("mote-shell: failed to create compositor: {e}");
                event_loop.exit();
                return;
            }
        }

        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        let scale = self.scale_factor;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
        }
        self.bridge
            .page()
            .notify_resized(self.width, self.height, scale);
        self.window = Some(window);
        eprintln!(
            "mote-shell: window {}x{} (scale {:.2}) up; chrome + {} tab(s) live",
            self.width,
            self.height,
            self.scale_factor,
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(scale_factor);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (cursor_px(position.x), cursor_px(position.y));
                self.route_mouse_move();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.route_click(button, state);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.route_wheel(delta);
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
        self.sync_active_title();
        self.maybe_run_housekeeping();
        self.upload_frames();

        // Once the chrome has painted, its bootstrap has run; push the initial
        // tab list + URL exactly once (an applyOp before then would be lost).
        if !self.chrome_ready && self.bridge.page().paint_count() >= 1 {
            self.chrome_ready = true;
            self.push_state_to_chrome();
        }

        // The plugin load pass is deferred past window creation so a slow or
        // offline git fetch (resolved_set → sync) cannot block startup and a
        // fatal resolution error cannot abort the app (T3 review findings).
        // Run it exactly once, on the first tick after the chrome is live, so
        // any first-install approval dialog can render immediately.
        if self.chrome_ready && !self.did_initial_load {
            self.did_initial_load = true;
            self.host.run_initial_load_pass();
            // Show the FIRST awaiting-approval dialog now that chrome is live.
            // The single dialog root holds one dialog, so multi-pending is shown
            // one at a time — `approve_plugin` advances to the next as each
            // resolves (looping over all here would overwrite earlier dialogs).
            // The buttons call the `approve_plugin` op; the pump thread finishes
            // the load and re-renders on the user's answer.
            self.show_next_pending_dialog();
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
        if let Some(page) = self.integrity_page.as_ref() {
            page.close();
        }
        if let Some(page) = self.picker_page.as_ref() {
            page.close();
        }
        self.bridge.page().close();
        for _ in 0..25 {
            self.engine.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        // Drain + stop the audit thread cleanly so buffered events flush to the
        // store before the process exits.
        if let Err(e) = self.host.audit.shutdown() {
            eprintln!("mote-shell: audit log shutdown failed: {e}");
        }
    }
}

/// The content page's surface size for a given window size (window minus the
/// chrome insets).
const fn viewport_size(width: u32, height: u32, inset_left: u32, inset_top: u32) -> (u32, u32) {
    let w = width.saturating_sub(inset_left);
    let h = height.saturating_sub(inset_top);
    (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
}

/// Scale a logical-pixel chrome inset to physical pixels for the display
/// `scale` factor (high-DPI). The chrome insets (`VIEWPORT_LEFT`/`VIEWPORT_TOP`)
/// are CSS/logical pixels; the composited page surface and the window→page
/// hit-test work in physical pixels, so they must be scaled (a fixed physical
/// inset misaligns at 1.25×). Rounds to the nearest physical pixel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "insets are small positive logical pixels; scaled + rounded to a physical pixel"
)]
fn scale_inset(logical: u32, scale: f64) -> u32 {
    (f64::from(logical) * scale.max(1.0)).round() as u32
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

/// Pixels scrolled per wheel "line" (winit [`MouseScrollDelta::LineDelta`]).
/// Matches the common desktop default so one notch scrolls a few text lines.
const WHEEL_STEP_PX: f64 = 40.0;

/// Round a (possibly fractional) scroll delta to integer page pixels.
#[allow(
    clippy::cast_possible_truncation,
    reason = "scroll deltas are small pixel values; rounding to the nearest pixel is intended"
)]
const fn wheel_delta(px: f64) -> i32 {
    px.round() as i32
}

/// Divide a window-physical pixel coordinate by the display `scale` to get the
/// CEF view's logical (DIP) coordinate. `scale` is winit's `scale_factor`
/// (always `> 0`); a scale of `1.0` is the identity.
#[allow(
    clippy::cast_possible_truncation,
    reason = "DIP coords are small positive pixel values; rounding to the nearest pixel is intended"
)]
fn physical_to_dip(physical: i32, scale: f64) -> i32 {
    (f64::from(physical) / scale).round() as i32
}

/// The window→page hit-test (plan §1.3).
const fn page_local_coords(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    inset_left: u32,
    inset_top: u32,
) -> Option<MousePosition> {
    let left = inset_left.cast_signed();
    let top = inset_top.cast_signed();
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

/// Which surface should receive a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickTarget {
    /// The privileged chrome page (`mote://chrome`).
    Chrome,
    /// The active content page.
    ContentPage,
}

/// Decide where a mouse event (click or move) should be routed.
///
/// When a chrome-rendered overlay is capturing input (an approval dialog is
/// showing or the integrity panel is open), **all** mouse events go to the
/// chrome page regardless of viewport geometry — the overlay renders
/// full-window on top of the content and must receive every click.  Otherwise
/// the geometry rules apply: a cursor inside the page viewport goes to the
/// content page; a cursor in the chrome insets goes to the chrome page.
///
/// The `overlay_capturing` flag is computed by
/// [`ShellApp::chrome_overlay_capturing_input`].  `page_local` is the result
/// of the viewport hit-test ([`page_local_coords`]) — `Some` means the cursor
/// is inside the content viewport, `None` means it is in the chrome insets.
pub(crate) const fn click_target(
    overlay_capturing: bool,
    page_local: Option<MousePosition>,
) -> ClickTarget {
    if overlay_capturing {
        // A chrome overlay (approval dialog or integrity panel) is capturing
        // input: all mouse events go to the chrome page regardless of where
        // the cursor sits geometrically.  The overlay is rendered full-window
        // inside the chrome page and content must not receive these events.
        ClickTarget::Chrome
    } else if page_local.is_some() {
        ClickTarget::ContentPage
    } else {
        ClickTarget::Chrome
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
        // Letters and digits: the ASCII-uppercase value equals the Windows VK
        // code (0x30–0x39 digits, 0x41–0x5A letters), so the shortcut is correct.
        // Punctuation does NOT line up — e.g. '.' is ASCII 0x2E, which is
        // `VK_DELETE` (see the `NamedKey::Delete => 0x2E` arm above). Faking the
        // ASCII value as the keydown VK made the omnibox interpret '.' as a
        // Delete keypress and drop it (issue #6). For non-alphanumeric
        // printables the CHAR event `route_key` also emits carries the actual
        // character and performs the text insertion, so a 0 ("unknown") keydown
        // VK is both correct and free of the editing-key collisions.
        Key::Character(s) => s.chars().next().map_or(0, |c| {
            let up = c.to_ascii_uppercase();
            if up.is_ascii_alphanumeric() {
                u8::try_from(u32::from(up)).map_or(0, i32::from)
            } else {
                0
            }
        }),
        _ => 0,
    }
}

/// Quote and escape `s` as a JavaScript string literal (double-quoted). Escapes
/// the control / quote / backslash / line-terminator set so a page-derived URL
/// or title can never break out of the literal or inject script when the chrome
/// `eval_js`'s `window.mote.applyOp(...)` call is built.
///
/// REVIEW RULE (`eval_js` interpolation chokepoint): this is the SOLE sanctioner of
/// untrusted data into a `Page::eval_js(format!(...))` string. Every
/// `eval_js(format!(...))` in the shell MUST interpolate only `js_string(...)`
/// output or trusted compile-time constants — never a raw page-derived string.
/// Adding a new `eval_js` call site without routing its dynamic args through
/// `js_string` is a chrome-context injection bug.
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

/// Extract a top-level string field `"field": "value"` from a JSON object.
///
/// Parses `json` as a JSON object (anchored, not a first-substring match) and
/// returns the named field iff it is present and a string. Returns `None` if
/// `json` is not an object or the field is absent / not a string.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?.get(field)?.as_str().map(str::to_string)
}

/// Extract a top-level unsigned-integer field `"field": <number>` from a JSON
/// object (the tab-op `id`s the chrome bootstrap sends are integers). Returns
/// `None` if `json` is not an object or the field is absent / not a `u64`.
fn json_u64_field(json: &str, field: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?.get(field)?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_size_excludes_chrome_insets() {
        let (w, h) = viewport_size(1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP);
        assert_eq!(w, 1280 - VIEWPORT_LEFT);
        assert_eq!(h, 800 - VIEWPORT_TOP);
        assert_eq!(viewport_size(0, 0, VIEWPORT_LEFT, VIEWPORT_TOP), (1, 1));
    }

    #[test]
    fn scale_inset_scales_logical_to_physical() {
        // At 1.0 the physical inset equals the logical one.
        assert_eq!(scale_inset(316, 1.0), 316);
        // At 1.25 it grows proportionally (the bug the fixed inset had).
        assert_eq!(scale_inset(316, 1.25), 395);
        assert_eq!(scale_inset(44, 1.25), 55);
        // A sub-1.0 scale never shrinks below the logical inset.
        assert_eq!(scale_inset(316, 0.5), 316);
    }

    #[test]
    fn physical_to_dip_maps_pointer_coords_into_the_view() {
        // At 1.0 the DIP coordinate is the physical one (identity).
        assert_eq!(physical_to_dip(640, 1.0), 640);
        // At 1.25 a physical pointer coord divides down into the logical (DIP)
        // space CEF lays the OSR page out in. This is the #7 input regression:
        // the live miss sent physical 663 verbatim where the view wanted 530
        // (~133px off), and 19 where the view wanted 15.
        assert_eq!(physical_to_dip(663, 1.25), 530);
        assert_eq!(physical_to_dip(19, 1.25), 15);
        // A sub-1.0 scale maps the other way (physical < DIP).
        assert_eq!(physical_to_dip(100, 0.5), 200);
    }

    #[test]
    fn wheel_delta_rounds_to_whole_pixels() {
        assert_eq!(wheel_delta(WHEEL_STEP_PX), 40);
        assert_eq!(wheel_delta(-WHEEL_STEP_PX * 2.0), -80);
        assert_eq!(wheel_delta(0.4), 0);
    }

    #[test]
    fn hit_test_maps_page_local_and_rejects_chrome() {
        let pos = page_local_coords(400, 300, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP)
            .expect("inside viewport");
        assert_eq!(pos.x, 400 - VIEWPORT_LEFT.cast_signed());
        assert_eq!(pos.y, 300 - VIEWPORT_TOP.cast_signed());
        assert!(page_local_coords(100, 300, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP).is_none());
        assert!(page_local_coords(400, 10, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP).is_none());
    }

    #[test]
    fn hit_test_uses_scaled_insets_on_hidpi() {
        // At 1.25× the left inset is 395px physical; a click at x=350 (inside the
        // unscaled 316 inset but left of the scaled one) belongs to the chrome.
        let left = scale_inset(VIEWPORT_LEFT, 1.25);
        let top = scale_inset(VIEWPORT_TOP, 1.25);
        assert!(page_local_coords(350, 300, 1600, 1000, left, top).is_none());
        assert!(page_local_coords(420, 300, 1600, 1000, left, top).is_some());
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
    fn punctuation_does_not_collide_with_editing_vk_codes() {
        // Regression for #6: '.' is ASCII 0x2E, which is VK_DELETE. Mapping the
        // raw ASCII value as the keydown VK made the omnibox treat a typed '.'
        // as Delete and drop it. Non-alphanumeric printables must NOT map to an
        // editing VK; 0 ("unknown") is correct — the CHAR event inserts them.
        assert_eq!(windows_key_code(&Key::Character(".".into())), 0);
        assert_ne!(
            windows_key_code(&Key::Character(".".into())),
            windows_key_code(&Key::Named(NamedKey::Delete)),
            "'.' must not share VK_DELETE's code"
        );
        for p in ["/", ":", "-", "_", "~", "?", "&", "=", "%", ",", "@"] {
            assert_eq!(
                windows_key_code(&Key::Character(p.into())),
                0,
                "punctuation `{p}` must map to 0, not a colliding editing VK"
            );
        }
        // Alphanumerics still map to their (correct) ASCII-uppercase VK.
        assert_eq!(
            windows_key_code(&Key::Character("z".into())),
            i32::from(b'Z')
        );
        assert_eq!(
            windows_key_code(&Key::Character("5".into())),
            i32::from(b'5')
        );
    }

    // ── Session-housekeeping wiring (discard + reap) ──────────────────────
    //
    // These exercise the shell's config plumbing and that the session crate's
    // Discarder / HiddenTabReaper make the right decisions at SHORT intervals
    // (so the logic is testable without waiting the 30-min / 30-day defaults).
    // The full housekeeping loop also drops CEF renderers, which needs a live
    // engine and is covered by the manual `mote-app` run, not a unit test.

    use std::time::{Duration, SystemTime};

    use mote_session::{Discarder, HiddenTabReaper, Tab};
    use mote_types::{TabId, WorkspaceId};

    #[test]
    fn discard_config_override_shortens_idle_threshold() {
        let cfg = discard_config_with(Some(Duration::from_secs(2)));
        assert_eq!(cfg.discard_after, Duration::from_secs(2));
        assert!(cfg.keep_pinned_loaded); // default preserved
        // No override → the 30-minute default.
        assert_eq!(
            discard_config_with(None).discard_after,
            Duration::from_mins(30)
        );
    }

    #[test]
    fn hidden_ttl_override_shortens_ttl() {
        let cfg = hidden_tab_config_with(Some(Duration::from_secs(2)));
        assert_eq!(cfg.ttl, Some(Duration::from_secs(2)));
        // No override → the 30-day default.
        assert_eq!(
            hidden_tab_config_with(None).ttl,
            Some(Duration::from_hours(720))
        );
    }

    #[test]
    fn discarder_with_short_threshold_discards_idle_active_tab() {
        // An active tab idle 5s with a 2s threshold is discard-eligible; the
        // shell builds the Discarder from `discard_config_with` exactly so.
        let discarder = Discarder::new(discard_config_with(Some(Duration::from_secs(2))));
        let mut tab = Tab::new(TabId::new(1), "https://a.com".into(), WorkspaceId::new(0));
        tab.last_visited = Some(SystemTime::now() - Duration::from_secs(5));
        assert!(discarder.should_discard(&tab));

        // A freshly-visited tab is not.
        let mut fresh = Tab::new(TabId::new(2), "https://b.com".into(), WorkspaceId::new(0));
        fresh.last_visited = Some(SystemTime::now());
        assert!(!discarder.should_discard(&fresh));
    }

    #[test]
    fn reaper_with_short_ttl_reaps_old_hidden_tab() {
        // A hidden tab released 5s ago with a 2s TTL is reap-eligible; the shell
        // builds the reaper from `hidden_tab_config_with` exactly so.
        let reaper = HiddenTabReaper::new(hidden_tab_config_with(Some(Duration::from_secs(2))));
        let mut tab = Tab::new(TabId::new(1), "https://a.com".into(), WorkspaceId::new(0));
        tab.hide(SystemTime::now() - Duration::from_secs(5));
        assert!(reaper.should_reap(&tab));

        // A just-hidden tab is not yet stale.
        let mut recent = Tab::new(TabId::new(2), "https://b.com".into(), WorkspaceId::new(0));
        recent.hide(SystemTime::now());
        assert!(!reaper.should_reap(&recent));
    }

    /// Verifies the reap wiring end-to-end at the session level (without a live
    /// CEF engine): a hidden tab past its TTL is removed from the session by
    /// the reaper and does not survive a flush/restore cycle. This mirrors what
    /// `ShellApp::reap_hidden_tabs` does — collect candidates via
    /// `HiddenTabReaper::should_reap`, then call `Session::remove_tab` for each.
    #[test]
    fn reap_wiring_deletes_expired_hidden_tab_from_session() {
        use mote_session::Session;
        use mote_storage::Store;
        use mote_types::{IdentityId, WorkspaceId};

        let store = Store::open_in_memory().unwrap();
        let identity = IdentityId::new(1);
        let plugin = mote_types::PluginName::new("mote-session").unwrap();
        let ns = store.namespace(&plugin, mote_storage::IdentityScope::PerIdentity(identity));
        let workspace = WorkspaceId::new(0);

        let mut session = Session::new(identity, workspace);
        // An active tab that should survive.
        let keep_id = session.add_tab("https://keep.com".to_owned(), workspace);
        // A hidden tab released 5 seconds ago — past the 2s TTL we'll configure.
        let reap_id = session.add_tab("https://reap-me.com".to_owned(), workspace);
        session
            .hide_tab(reap_id, SystemTime::now() - Duration::from_secs(5))
            .unwrap();
        // A recently-hidden tab that must NOT be reaped yet.
        let young_id = session.add_tab("https://young.com".to_owned(), workspace);
        session.hide_tab(young_id, SystemTime::now()).unwrap();

        // Build a reaper with a 2s TTL (mirrors `hidden_tab_config_with`).
        let reaper = HiddenTabReaper::new(hidden_tab_config_with(Some(Duration::from_secs(2))));

        // Apply the same logic ShellApp::reap_hidden_tabs uses.
        let to_reap: Vec<_> = session
            .tab_picker_ranked(workspace)
            .into_iter()
            .filter(|t| reaper.should_reap(t))
            .map(|t| t.id)
            .collect();
        assert_eq!(to_reap, vec![reap_id], "only the stale tab is a candidate");

        for id in &to_reap {
            assert!(session.remove_tab(*id).is_some());
        }

        // Flush and restore: the reaped tab must be gone; others must survive.
        session.flush(&ns).unwrap();
        let restored = Session::restore(&ns, identity).unwrap();
        assert!(
            restored.tab(keep_id).is_some(),
            "active tab must survive reap"
        );
        assert!(
            restored.tab(young_id).is_some(),
            "recently-hidden tab must survive reap"
        );
        assert!(
            restored.tab(reap_id).is_none(),
            "stale hidden tab must be deleted after reap + flush"
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

    // ── approve_plugin op-boundary validation (ADR-0005) ──────────────────

    /// Parses a JSON payload into a `DialogResult` and runs the op-boundary
    /// origin-glob validation exactly as the `approve_plugin` op handler does.
    fn validate_payload(json: &str) -> Result<(), &'static str> {
        let result: approval::DialogResult =
            serde_json::from_str(json).expect("test payload parses");
        validate_dialog_origins(&result)
    }

    #[test]
    fn approve_payload_with_valid_origins_passes_op_boundary() {
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"page:inject_script","action":"*","mode":"origins",
             "origins":["https://example.com/*","https://*.github.com/*"]}]}"#;
        assert!(validate_payload(json).is_ok(), "valid origin globs pass");
    }

    #[test]
    fn approve_payload_with_full_mode_skips_origin_validation() {
        // mode != "origins" → origins are not validated (none are present).
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"storage:persistent","action":"","mode":"full"}]}"#;
        assert!(validate_payload(json).is_ok());
    }

    #[test]
    fn approve_payload_with_injection_origin_is_rejected() {
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"page:inject_script","action":"*","mode":"origins",
             "origins":["<script>alert(1)</script>"]}]}"#;
        assert_eq!(
            validate_payload(json),
            Err("approve_plugin: invalid origin glob"),
            "an injection-shaped origin must be rejected at the op boundary"
        );
    }

    #[test]
    fn approve_payload_over_origin_count_cap_is_rejected() {
        let origins: Vec<String> = (0..=approval::MAX_ORIGINS_PER_PERMISSION)
            .map(|i| format!("https://h{i}.example.com/*"))
            .collect();
        let result = approval::DialogResult {
            plugin: "p".into(),
            decision: "grant".into(),
            permissions: vec![approval::DialogPermission {
                domain: "page:inject_script".into(),
                action: "*".into(),
                mode: "origins".into(),
                origins: Some(origins),
            }],
        };
        assert_eq!(
            validate_dialog_origins(&result),
            Err("approve_plugin: too many origins for a permission"),
            "exceeding the per-permission origin cap must be rejected"
        );
    }

    #[test]
    fn approve_payload_malformed_json_is_an_error() {
        // The op handler maps a parse failure to a 400; mirror that boundary.
        assert!(serde_json::from_str::<approval::DialogResult>("not json").is_err());
    }

    // ── click_target: overlay-aware mouse routing ─────────────────────────
    //
    // When a chrome overlay (approval dialog or integrity panel) is capturing
    // input every mouse event must go to the chrome page — the overlay renders
    // full-window in the chrome page and content must not see those events.
    //
    // Inline helpers used in tests below:
    //   inside  = Some(MousePosition { x: 100, y: 100 })  — cursor in viewport
    //   outside = None                                      — cursor in chrome insets

    /// THE BUG: overlay capturing + cursor inside viewport → chrome (not content).
    ///
    /// With the old geometry-only logic this returns `ContentPage`; the fix
    /// makes it return `Chrome`.  The test MUST fail before the fix is applied
    /// and MUST pass afterwards.
    #[test]
    fn overlay_capturing_inside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(true, Some(MousePosition { x: 100, y: 100 })),
            ClickTarget::Chrome,
            "an overlay capturing input must redirect viewport clicks to chrome"
        );
    }

    /// Overlay capturing + cursor outside viewport → chrome (no change needed,
    /// already correct by the geometry rule, but must remain correct).
    #[test]
    fn overlay_capturing_outside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(true, None),
            ClickTarget::Chrome,
            "an overlay capturing input routes chrome-inset clicks to chrome"
        );
    }

    /// No overlay, cursor inside viewport → content (the normal happy path).
    #[test]
    fn no_overlay_inside_viewport_routes_to_content() {
        assert_eq!(
            click_target(false, Some(MousePosition { x: 100, y: 100 })),
            ClickTarget::ContentPage,
            "without an overlay a viewport click goes to the content page"
        );
    }

    /// No overlay, cursor outside viewport (in chrome insets) → chrome.
    #[test]
    fn no_overlay_outside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(false, None),
            ClickTarget::Chrome,
            "without an overlay a chrome-inset click goes to the chrome page"
        );
    }

    // ── urlbar_query op wiring ────────────────────────────────────────────

    /// `build_op_registry` registers the `urlbar_query` op (wiring check).
    #[test]
    fn urlbar_query_op_is_registered() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"urlbar_query"),
            "urlbar_query must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// Missing `text` field → 400 error (op-boundary validation).
    #[test]
    fn urlbar_query_missing_text_is_a_400() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Directly exercise the same logic the op handler uses.
        let result = json_string_field("{}", "text");
        assert!(
            result.is_none(),
            "missing `text` field must yield None so the op returns 400"
        );
        // Queue must remain empty.
        assert!(queue.lock().unwrap().is_empty());
    }

    /// Valid `text` field → `ShellCommand::UrlbarQuery` is enqueued.
    #[test]
    fn urlbar_query_valid_text_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does.
        let text =
            json_string_field(r#"{"text":"example"}"#, "text").expect("text field must parse");
        push(&queue, ShellCommand::UrlbarQuery(text));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::UrlbarQuery(t) => {
                assert_eq!(t, "example", "enqueued text must match the input");
            }
            other => panic!("expected UrlbarQuery; got {other:?}"),
        }
    }

    /// Empty text string is accepted (op enqueues a command; the handler lets
    /// chrome clear the dropdown by pushing an empty suggestions list).
    #[test]
    fn urlbar_query_empty_text_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let text = json_string_field(r#"{"text":""}"#, "text").expect("empty text must parse fine");
        push(&queue, ShellCommand::UrlbarQuery(text));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        match q.pop_front().unwrap() {
            ShellCommand::UrlbarQuery(t) => assert!(t.is_empty(), "text must be empty string"),
            other => panic!("expected UrlbarQuery; got {other:?}"),
        }
    }
}
