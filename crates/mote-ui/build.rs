//! Build-time hook: tell cargo to recompile mote-ui whenever any file in the
//! `chrome/` directory changes.
//!
//! `include_str!` for individual files IS tracked by cargo, but a full
//! directory's worth of `include_str!` calls (and any future move to
//! `include_dir!`) is more robustly handled by an explicit
//! `cargo:rerun-if-changed`. This makes "chrome edit → rebuild" reliable
//! without depending on cargo correctly tracking each embedded file.
//!
//! Same class of build-cache footgun as `crates/mote-pluginmgr/build.rs`.

fn main() {
    println!("cargo:rerun-if-changed=chrome");
}
