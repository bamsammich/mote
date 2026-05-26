//! CEF subprocess helper. CEF re-execs this binary for the renderer, GPU, and
//! utility processes. It calls `execute_process` with the SAME custom App as the
//! browser process so the RENDERER subprocess installs the bridge's
//! RenderProcessHandler (which gates window.cefQuery to the chrome document).
use cef::{api_hash, args::Args, execute_process, sys};

#[path = "../bridge.rs"]
mod bridge;

fn main() {
    let args = Args::new();
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let mut app = bridge::make_app();
    execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
}
