//! Plugin provenance — how a plugin's code reached the user's machine.
//!
//! [`Provenance`] is the minimal, fieldless classification the approval-flow
//! brain (Task 2) needs to decide auto-grant vs. dialog. Richer data (directory
//! paths, resolved source strings) lives in `ResolvedPlugin` (Task 3) and is not
//! needed at classification time.

/// How a plugin's code reached the user's machine.
///
/// Used by the approval-flow classifier to decide whether a plugin requires a
/// user-facing dialog (`Bundled` and `DevMode` are auto-granted; all others go
/// through the store + hash comparison logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// First-party plugin embedded in the Mote binary bundle.
    ///
    /// Trusted by construction: ships with the release and cannot be altered by
    /// a third party without replacing the binary.
    Bundled,

    /// Plugin declared via `mote.dev_mode { directories = … }`.
    ///
    /// The developer's own code running in-place; auto-approved because the
    /// developer made an explicit opt-in gesture.
    DevMode,

    /// Plugin declared in `plugins.lua` with a `github:` or `git+https:` source.
    ///
    /// Requires approval on first install and on any permission-expanding update.
    DeclaredGit,

    /// Plugin declared in `plugins.lua` with a `path:` source.
    ///
    /// Requires approval on first install and on any permission-expanding update.
    Path,

    /// Plugin found under `<config>/plugins/<name>/` but not listed in the
    /// spec set (detected implicitly in Task 6's scanner).
    ///
    /// Requires approval on first install and on any permission-expanding update.
    ImplicitLocal,
}
