//! The Mote binary.
//!
//! `main` runs the CEF process-split shim **first** ([`mote_cef::bootstrap_with_bridge`]):
//! Chromium's multi-process model re-execs this same binary for each subprocess
//! (renderer, GPU, utility), and a subprocess must do nothing but run CEF and
//! exit. Only the **browser** process proceeds past the shim.
//!
//! After the subprocess check the binary inspects the process arguments:
//!
//! - If the first non-program argument is `plugin` or `secrets`, the
//!   `mote-cli` surface handles the request and returns its exit code without
//!   opening a window. This is the Phase-3 management path.
//! - Otherwise the browser process boots [`mote_shell::run`] — the window,
//!   compositor, and event loop.
//!
//! The CEF subprocess shim runs first in both paths: a CLI invocation is the
//! browser-role process and never spawns renderers, so `bootstrap_with_bridge`
//! returns `Browser` and the dispatch falls through to the CLI check.
//!
//! The bridge variant of bootstrap is used (not plain `bootstrap`) so the
//! subprocess installs the privileged-chrome renderer origin gate and declares
//! the `mote://chrome` scheme (ADR-0005) — the host bridge the shell wires
//! depends on both being present in the renderer subprocess.

use std::process::ExitCode;

use mote_cef::ProcessRole;

fn main() -> ExitCode {
    // STEP 1: the process split. In a CEF subprocess this runs CEF's child logic
    // and returns `Subprocess`; we must exit immediately with its code, doing
    // nothing else (no window, no shell).
    match mote_cef::bootstrap_with_bridge() {
        ProcessRole::Subprocess { exit_code } => {
            return ExitCode::from(u8::try_from(exit_code.clamp(0, 255)).unwrap_or(0));
        }
        ProcessRole::Browser => {}
    }

    // STEP 1.5: management subcommand dispatch.
    //
    // If the first non-program argument is `plugin` or `secrets`, hand off to
    // the CLI surface and return immediately. This lets `mote plugin sync` (etc.)
    // run without opening a window. The CEF shim above runs first so the
    // subprocess split is always honoured.
    let first_arg = std::env::args_os().nth(1);
    let is_management = first_arg.as_deref().is_some_and(|a| {
        let s = a.to_string_lossy();
        s == "plugin" || s == "secrets"
    });
    if is_management {
        return mote_cli::run(std::env::args_os());
    }

    // STEP 2: the browser process boots the shell (window + compositor + loop).
    match mote_shell::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mote: shell exited with error: {e}");
            ExitCode::FAILURE
        }
    }
}
