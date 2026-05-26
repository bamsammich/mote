//! Disposable scaffold crate.
//!
//! This crate exists only to exercise the mise / rustfmt / clippy / lefthook
//! toolchain end to end while the real crate architecture (see `DESIGN.md` and
//! `ROADMAP.md` Phase 1) awaits its design spec. Delete it once real crates
//! exist; the workspace lint policy in the root `Cargo.toml` applies to them
//! automatically via `[lints] workspace = true`.

/// Returns the canonical greeting used to smoke-test the build pipeline.
#[must_use]
pub const fn greeting() -> &'static str {
    "mote toolchain online"
}

#[cfg(test)]
mod tests {
    use super::greeting;

    #[test]
    fn greeting_is_stable() {
        assert_eq!(greeting(), "mote toolchain online");
    }
}
