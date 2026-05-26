//! The CEF multi-process split.
//!
//! CEF is multi-process: one browser process plus renderer/GPU/utility
//! subprocesses. The idiomatic `cef-rs` pattern is a **single binary that
//! re-execs itself** (validated by the spike, docs/research/ui-spike-cef-html.md
//! §1): `execute_process` is called early; in a subprocess invocation it runs
//! the CEF subprocess loop and returns `0`, and the caller must exit. In the
//! browser invocation it returns `-1` and the caller proceeds to
//! [`crate::Engine::init`].
//!
//! `mote-cef` owns this split entirely so `mote-app`'s `main` never names a raw
//! `execute_process` call.
#![allow(
    unsafe_code,
    reason = "execute_process / api_hash are CEF FFI; contained per DISCIPLINES.md §1"
)]

use cef::{App, ImplCommandLine, api_hash, args::Args, execute_process, sys};

/// What role the current process plays, decided by [`bootstrap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// This is the main browser process. The caller should continue and call
    /// [`crate::Engine::init`].
    Browser,
    /// This is a CEF subprocess (renderer/GPU/utility). `execute_process` has
    /// already run the subprocess loop; the caller MUST exit immediately with
    /// the carried exit code and do nothing else.
    Subprocess {
        /// The process exit code CEF's subprocess loop returned.
        exit_code: i32,
    },
}

/// Initialise CEF's process model and decide this process's role.
///
/// Call this as the **very first thing** in `main`, before any other work:
///
/// ```no_run
/// use mote_cef::ProcessRole;
///
/// fn main() -> std::process::ExitCode {
///     match mote_cef::bootstrap() {
///         ProcessRole::Subprocess { exit_code } => {
///             // A CEF helper invocation — exit now, do nothing else.
///             std::process::ExitCode::from(exit_code as u8)
///         }
///         ProcessRole::Browser => {
///             // Main process — bring up the engine and run the app.
///             std::process::ExitCode::SUCCESS
///         }
///     }
/// }
/// ```
///
/// Because the same binary serves as both the main process and the CEF helper,
/// no separate helper executable is required.
#[must_use]
pub fn bootstrap() -> ProcessRole {
    // Pin the API hash to the version the bindings were generated against. CEF
    // requires this before any other API call in the process.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();

    // CEF passes `--type=<role>` to every subprocess it spawns; its absence
    // identifies the browser (main) process.
    let is_browser = args
        .as_cmd_line()
        .is_none_or(|cmd| cmd.has_switch(Some(&cef::CefString::from("type"))) != 1);

    // In a subprocess this runs CEF's subprocess loop and returns its exit code;
    // in the browser process it returns -1 immediately. We pass no `App` —
    // browser-process-only handlers are configured in `Engine::init`, and
    // subprocesses don't need them for the OSR v0.1 path.
    let ret = execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );

    if is_browser {
        debug_assert_eq!(ret, -1, "browser process: execute_process must return -1");
        ProcessRole::Browser
    } else {
        ProcessRole::Subprocess { exit_code: ret }
    }
}

/// Like [`bootstrap`], but installs the **host-bridge** renderer handler in CEF
/// subprocesses, gated to `chrome_url` (ADR-0005 isolation layer 1).
///
/// Use this entry point instead of [`bootstrap`] whenever the app will create a
/// [`crate::HostBridge`]. The renderer-side router that installs
/// `window.cefQuery` runs in a CEF *subprocess*, so the gating
/// `RenderProcessHandler` must be installed via the `App` passed to
/// `execute_process` here — not only via [`crate::Engine::init`] in the browser
/// process. Pass the **same** `chrome_url` to both this function and
/// [`crate::EngineConfig::chrome_url`]; that single URL is the only document the
/// privileged binding is ever installed for.
///
/// Returns the same [`ProcessRole`] contract as [`bootstrap`]: in a subprocess,
/// `execute_process` has already run the gated renderer loop — exit immediately.
#[must_use]
pub fn bootstrap_with_bridge(chrome_url: &str) -> ProcessRole {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let is_browser = args
        .as_cmd_line()
        .is_none_or(|cmd| cmd.has_switch(Some(&cef::CefString::from("type"))) != 1);

    // Install the gated host-bridge App so the renderer subprocess registers the
    // URL-gated RenderProcessHandler. The gate makes installing the binding
    // without the chrome-URL check impossible — there is no ungated app.
    let mut app = crate::bridge::render_process_app(chrome_url);
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );

    if is_browser {
        debug_assert_eq!(ret, -1, "browser process: execute_process must return -1");
        ProcessRole::Browser
    } else {
        ProcessRole::Subprocess { exit_code: ret }
    }
}
