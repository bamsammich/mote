//! CEF isolation wrapper for Mote.
//!
//! This is the **only** crate permitted to depend on `cef`/`cef_rs`
//! (DISCIPLINES.md §1, CEF upgrade discipline). It wraps the Chromium Embedded
//! Framework behind safe, Mote-shaped Rust types so the rest of the codebase
//! never names a `cef::` type and a CEF upgrade's breakage is contained here.
//!
//! # Architecture
//!
//! - [`bootstrap`] / [`ProcessRole`] — the `execute_process` re-exec split that
//!   makes one binary serve as both the browser process and CEF subprocesses.
//! - [`Engine`] / [`EngineConfig`] — RAII lifecycle for the CEF runtime
//!   (initialise, pump the message loop, shut down).
//! - [`Page`] / [`PageOptions`] / [`PageRole`] — an off-screen browser (a Mote
//!   tab): create (optionally under a [`ProfileHandle`] and with a privileged
//!   chrome vs untrusted content role), navigate, history, inject input, and read
//!   painted [`PaintFrame`]s.
//! - [`ProfileHandle`] / [`ProfileManager`] / [`IdentityId`] — per-identity
//!   browsing profiles. A Mote identity is one Chromium `RequestContext` with an
//!   isolated on-disk storage path (see `docs/identity-isolation.md`).
//! - [`MousePosition`] / [`MouseButton`] / [`KeyInput`] / [`Modifiers`] et al. —
//!   the CEF-free input vocabulary `Page`'s `send_*` methods inject.
//! - [`ResourceInterceptor`] / [`RequestInfo`] / [`RequestDecision`] — the
//!   network-interception seam ad-block / privacy plugins ride on.
//! - [`HostBridge`] / [`OpRegistry`] / [`OpHandler`] / [`OpResponse`] /
//!   [`ChromePage`] / [`ChromePageRequest`] — the privileged chrome↔Rust
//!   transport (ADR-0005). `window.mote.invoke(op, params)` over the CEF message
//!   router, dispatched to a closed set of structured ops, scoped to the chrome
//!   browser in two independent layers (renderer origin gate + chrome-only
//!   router), made **unrepresentable to misconfigure** by construction.
//! - [`ChromeResources`] / [`CHROME_ORIGIN`] / [`chrome_url`] — the privileged
//!   internal `mote://chrome` scheme that serves the chrome assets (ADR-0005
//!   amendment). The host registers `path -> (bytes, content-type)` resources;
//!   `mote-cef` serves them from `mote://chrome/...`. The host-bridge gates on the
//!   fixed [`CHROME_ORIGIN`] origin constant, not a runtime URL — web content is
//!   `http(s)` and can never carry that origin, so the gate is unforgeable.
//!
//! # Off-screen rendering
//!
//! v0.1 uses CEF's CPU `on_paint` path (BGRA buffers), the deterministic,
//! ANGLE-independent Linux baseline validated by the UI spike. Accelerated
//! shared-texture OSR (`--use-angle=gl-egl` + `--ozone-platform`) is a future
//! optimisation and a documented risk (see docs/research/ui-spike-cef-html.md).
//!
//! # Unsafe & FFI
//!
//! `unsafe` is required for the CEF C ABI and is allowed ONLY in this crate,
//! narrowly per-module with justification. Every CEF→Rust callback body is
//! wrapped in `catch_unwind` (the release profile is `panic = "abort"`, so an
//! unwind across the C ABI would be UB). See the `ffi` module.

mod bridge;
mod browser;
mod engine;
mod error;
mod ffi;
mod input;
mod interceptor;
mod paint;
mod process;
mod profile;
mod scheme;

pub use bridge::{HostBridge, OpHandler, OpRegistry, OpResponse};
pub use browser::{
    ChromePage, ChromePageRequest, ContextMenuKind, ContextMenuRequest, FindResult, Page,
    PageOptions, PageRole, PopupTabRequest, edit_flag,
};
pub use engine::{Engine, EngineConfig};
pub use error::{CefError, Result};
pub use input::{ButtonAction, KeyAction, KeyInput, Modifiers, MouseButton, MousePosition};
pub use interceptor::{AllowAll, RequestDecision, RequestInfo, ResourceInterceptor};
pub use paint::{PaintFrame, PixelFormat};
pub use process::{ProcessRole, bootstrap, bootstrap_with_bridge};
pub use profile::{IdentityId, ProfileHandle, ProfileManager};
pub use scheme::{
    CHROME_HOST, CHROME_ORIGIN, CHROME_SCHEME, ChromeResources, OVERLAY_HOST, OVERLAY_ORIGIN,
    chrome_url, overlay_url,
};

#[cfg(test)]
mod guard_test {
    //! Enforces DISCIPLINES.md §1: `use cef` / `use cef_rs` must not appear in
    //! any workspace crate other than `mote-cef`. This is the in-tree half of
    //! the CI guard; it fails the build if the isolation boundary is breached.

    use std::path::{Path, PathBuf};

    /// Locate the workspace `crates/` directory from this crate's manifest dir.
    fn crates_dir() -> PathBuf {
        // CARGO_MANIFEST_DIR = <workspace>/crates/mote-cef
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("mote-cef has a parent dir")
            .to_path_buf()
    }

    /// Recursively collect `.rs` files under `dir`, skipping `target/`.
    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Returns `true` if a source line imports `cef` / `cef_rs` directly. Comment
    /// lines are ignored so prose mentioning CEF (every design doc does) is fine.
    fn line_breaches_boundary(line: &str) -> bool {
        const NEEDLES: [&str; 4] = ["use cef ", "use cef::", "use cef_rs", "extern crate cef"];
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            return false;
        }
        if NEEDLES.iter().any(|n| trimmed.contains(n)) {
            return true;
        }
        // Catch `cef::Foo` / `cef_rs::Foo` path usage, but not `mote_cef::`.
        for token in [" cef::", "(cef::", "<cef::", "&cef::", "cef_rs::"] {
            if line.contains(token) {
                return true;
            }
        }
        false
    }

    #[test]
    fn no_direct_cef_imports_outside_mote_cef() {
        let crates = crates_dir();
        let mut sources = Vec::new();

        let Ok(entries) = std::fs::read_dir(&crates) else {
            panic!("cannot read crates dir {}", crates.display());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // mote-cef is the sanctioned home of cef; skip it.
            if !path.is_dir() || name == "mote-cef" {
                continue;
            }
            rust_sources(&path.join("src"), &mut sources);
            for sub in ["examples", "tests", "benches"] {
                rust_sources(&path.join(sub), &mut sources);
            }
        }

        let mut violations = Vec::new();
        for file in &sources {
            let Ok(contents) = std::fs::read_to_string(file) else {
                continue;
            };
            for (i, line) in contents.lines().enumerate() {
                if line_breaches_boundary(line) {
                    violations.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "DISCIPLINES.md §1 violated: `cef`/`cef_rs` used outside mote-cef:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn boundary_matcher_recognizes_violations() {
        assert!(line_breaches_boundary("use cef::Browser;"));
        assert!(line_breaches_boundary("use cef_rs::sys;"));
        assert!(line_breaches_boundary(
            "    let b: Option<cef::Browser> = None;"
        ));
        assert!(line_breaches_boundary("extern crate cef;"));
    }

    #[test]
    fn boundary_matcher_allows_innocent_lines() {
        assert!(!line_breaches_boundary(
            "// the cef::Browser type is wrapped here"
        ));
        assert!(!line_breaches_boundary("use mote_cef::Page;"));
        assert!(!line_breaches_boundary(
            "let engine = mote_cef::Engine::init(&cfg)?;"
        ));
        assert!(!line_breaches_boundary(
            "//! mote-cef wraps cef behind safe types"
        ));
    }
}
