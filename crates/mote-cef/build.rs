//! Build script for `mote-cef`.
//!
//! `cef-dll-sys` copies the CEF runtime (`libcef.so`, `*.pak`, `icudtl.dat`,
//! `locales/`, the V8 snapshot, and the ANGLE libs) into the Cargo profile
//! directory next to the produced binary, but it does **not** emit an rpath
//! (the spike documented this as the one packaging wart: `LD_LIBRARY_PATH` was
//! required at runtime — see docs/research/ui-spike-cef-html.md §1).
//!
//! We close that gap here by adding `$ORIGIN` to the binary's runpath so the
//! dynamic loader resolves `libcef.so` from the directory the executable lives
//! in — no `LD_LIBRARY_PATH` needed for dev binaries, examples, or shipped
//! bundles where the CEF runtime sits beside the binary.

fn main() {
    // Linux/ELF only. macOS/Windows resolve the framework/DLL differently and
    // are out of scope for the v0.1 Linux target (DESIGN: Linux x86_64 first).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // `$ORIGIN` must reach the linker literally; escape `$` from the shell
        // word-splitting cargo does NOT do, but keep the `$` for ld. Using the
        // `-rpath` (not `-rpath-link`) form bakes it into DT_RUNPATH.
        println!("cargo::rustc-link-arg=-Wl,-rpath,$ORIGIN");
        // Examples and tests live one directory deeper (`target/<profile>/examples/`),
        // while the CEF runtime is copied to `target/<profile>/`. Add the parent so
        // example/test binaries resolve `libcef.so` too.
        println!("cargo::rustc-link-arg=-Wl,-rpath,$ORIGIN/..");
    }

    println!("cargo::rerun-if-changed=build.rs");
}
