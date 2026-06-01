//! Build-time hook: tell cargo to recompile mote-pluginmgr whenever any file
//! in the repository's `plugins/` tree changes.
//!
//! `include_dir!` (the third-party macro used to embed the bundle in
//! `src/bundle.rs`) does NOT automatically emit `cargo:rerun-if-changed` for
//! the embedded directory. Without this hook, plugin-file edits leave cargo's
//! incremental build cache unaware, the binary ships a stale embed, and the
//! runtime then serves the stale code — a footgun we hit repeatedly during
//! Phase 5a development. This build script closes that gap.

fn main() {
    println!("cargo:rerun-if-changed=../../plugins");
}
