//! The Mote binary.
//!
//! `main` runs the CEF process-split shim **first** ([`mote_cef::bootstrap_with_bridge`]):
//! Chromium's multi-process model re-execs this same binary for each subprocess
//! (renderer, GPU, utility), and a subprocess must do nothing but run CEF and
//! exit. Only the **browser** process proceeds to boot [`mote_shell::run`] — the
//! window, the compositor, and the event loop.
//!
//! The bridge variant of bootstrap is used (not plain `bootstrap`) so the
//! subprocess installs the privileged-chrome renderer origin gate and declares
//! the `mote://chrome` scheme (ADR-0005) — the host bridge the shell wires
//! depends on both being present in the renderer subprocess.
//!
//! Management subcommands (the `mote-cli` path) are Phase 3; for now the browser
//! is the only boot path.

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

    // STEP 2: the browser process boots the shell (window + compositor + loop).
    match mote_shell::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mote: shell exited with error: {e}");
            ExitCode::FAILURE
        }
    }
}
