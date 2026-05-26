//! CEF subprocess helper. CEF re-execs this binary for the renderer, GPU, and
//! utility processes. It must call `execute_process` and then exit — it never
//! initializes CEF or runs the message loop.
use cef::{api_hash, args::Args, execute_process, sys, App};

fn main() {
    let args = Args::new();
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
}
