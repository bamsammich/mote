//! CEF engine lifecycle: settings, initialise, message-loop pumping, shutdown.
//!
//! [`Engine`] is a single-instance RAII handle for the CEF runtime. Constructing
//! it via [`Engine::init`] runs `cef::initialize`; dropping it (or calling
//! [`Engine::shutdown`]) runs `cef::shutdown`. CEF is process-global, so only one
//! `Engine` may exist at a time — enforced by an atomic guard.
#![allow(
    unsafe_code,
    reason = "initialize / shutdown / do_message_loop_work are CEF FFI; contained per DISCIPLINES.md §1"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use cef::{CefString, Settings, args::Args, do_message_loop_work, initialize, shutdown};

use crate::error::{CefError, Result};

/// Guards against constructing more than one [`Engine`]: CEF's runtime is
/// process-global and cannot be initialised twice.
static ENGINE_LIVE: AtomicBool = AtomicBool::new(false);

/// Tunables for CEF initialisation. Defaults match the spike's validated OSR
/// configuration; callers override only what they need.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Directory CEF uses for its cache/profile. Defaults to `.mote-cef-cache`
    /// under the current working directory. A stable path silences the
    /// `root_cache_path` startup warning the spike documented.
    pub cache_path: PathBuf,
    /// Disable the Chromium sandbox. Defaults to `false` (sandbox ON) to match
    /// the DESIGN security model — Chromium's renderer sandbox isolates web
    /// content. Callers that genuinely need the sandbox off (CI, headless dev,
    /// the `osr_smoke` example) must set this to `true` explicitly.
    pub no_sandbox: bool,
    /// Drive CEF's work via [`Engine::pump`] / [`Engine::run`] rather than CEF's
    /// own internal message loop. Required for the OSR compositor model.
    pub external_message_pump: bool,
    /// The privileged **chrome document URL** the host bridge is scoped to
    /// (ADR-0005). When `Some(url)`, `Engine::init` installs the custom CEF `App`
    /// whose renderer-side `RenderProcessHandler` gates the `window.cefQuery` /
    /// `window.mote` binding to exactly this URL (isolation layer 1). When `None`
    /// (the default) no bridge `App` is installed and no page can ever receive
    /// the binding.
    ///
    /// **The same URL must be passed to [`crate::bootstrap_with_bridge`]** so the
    /// renderer *subprocess* installs the identical gate. Passing it in only one
    /// place leaves the gate uninstalled in the other process; both `mote-cef`
    /// entry points take it so the host wires one constant into both.
    pub chrome_url: Option<String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            cache_path: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".mote-cef-cache"),
            no_sandbox: false,
            external_message_pump: true,
            chrome_url: None,
        }
    }
}

/// A live CEF runtime. See module docs.
#[derive(Debug)]
pub struct Engine {
    /// Set to `false` once `shutdown` has run, so `Drop` doesn't double-shutdown.
    live: bool,
}

impl Engine {
    /// Initialise the CEF runtime with the given configuration.
    ///
    /// Must be called in the **browser process only** (after [`crate::bootstrap`]
    /// returned [`crate::ProcessRole::Browser`]). Windowless rendering is always
    /// enabled — this wrapper is OSR-first per ADR 0003.
    ///
    /// # Errors
    /// Returns [`CefError::Lifecycle`] if an `Engine` already exists, or
    /// [`CefError::Initialize`] if `cef::initialize` fails.
    pub fn init(config: &EngineConfig) -> Result<Self> {
        if ENGINE_LIVE.swap(true, Ordering::SeqCst) {
            return Err(CefError::Lifecycle("Engine already initialized"));
        }

        if let Err(e) = std::fs::create_dir_all(&config.cache_path) {
            eprintln!(
                "mote-cef: could not create cache dir {}: {e}",
                config.cache_path.display()
            );
        }

        let settings = Settings {
            windowless_rendering_enabled: 1,
            external_message_pump: i32::from(config.external_message_pump),
            no_sandbox: i32::from(config.no_sandbox),
            root_cache_path: CefString::from(&*config.cache_path.to_string_lossy()),
            ..Default::default()
        };

        let args = Args::new();
        // NOTE (future risk, docs/research/ui-spike-cef-html.md §1 / §8): accelerated
        // zero-copy OSR on Linux needs `--use-angle=gl-egl` + `--ozone-platform`.
        // v0.1 deliberately uses the CPU `on_paint` fallback, which needs no GPU
        // command-line switches, so none are injected here.
        //
        // When a chrome URL is configured, install the host-bridge `App` so the
        // browser-process renderer handler exists and the renderer-side URL gate
        // (isolation layer 1) is wired. With no chrome URL, no `App` is passed and
        // no process can ever receive the privileged binding.
        let mut bridge_app = config
            .chrome_url
            .as_deref()
            .map(crate::bridge::render_process_app);
        let ok = initialize(
            Some(args.as_main_args()),
            Some(&settings),
            bridge_app.as_mut(),
            std::ptr::null_mut(),
        );

        if ok == 1 {
            Ok(Self { live: true })
        } else {
            ENGINE_LIVE.store(false, Ordering::SeqCst);
            Err(CefError::Initialize)
        }
    }

    /// Pump one slice of CEF work. Call this from the host event loop when using
    /// an external message pump (the OSR model). Drives `on_paint`, network IO,
    /// and navigation callbacks.
    pub fn pump(&self) {
        do_message_loop_work();
    }

    /// Shut the CEF runtime down explicitly. Idempotent; also run by `Drop`.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        if self.live {
            shutdown();
            self.live = false;
            ENGINE_LIVE.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}
