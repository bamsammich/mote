//! Secret subsystem for Mote.
//!
//! Owns typed secret definitions ([`SecretDef`] + [`BackendKind`]), the five
//! backend implementations (`env`, `file`, `age`, and stubs for `keyring` /
//! `password-manager`), the [`SecretResolver`] dispatcher, and the
//! [`SecretProviderRouter`] callback trait (implemented by `mote-runtime` to
//! break the potential dependency cycle).
//!
//! # Security contract
//!
//! Secret values **always** live in [`SecretValue`] (`secrecy::SecretString`),
//! which is zeroized on drop and redacts `Debug` output. Nothing in this crate
//! logs, displays, or returns a bare `String` containing a secret.
//!
//! This crate does **not** parse `secrets.lua` — that is `mote-lua`'s job.
//! It converts already-parsed [`mote_lua`]-style raw entries into typed
//! [`SecretDef`]s and resolves them.

pub mod backend;
pub mod def;
pub mod resolver;

pub use def::{BackendKind, SecretDef};
pub use resolver::{SecretProviderRouter, SecretResolver};
pub use secrecy::SecretString;

use std::path::PathBuf;

/// The resolved value handed to a plugin or caller. Zeroized on drop;
/// `Debug` output is redacted (`SecretString("[REDACTED]")`).
pub type SecretValue = SecretString;

/// All errors that can arise when resolving a named secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// No definition exists for the given secret name.
    #[error("secret not found: {name}")]
    NotFound {
        /// The name that was not found.
        name: String,
    },

    /// The backend is not available (e.g. keyring daemon not running,
    /// no PM router configured, or the variant is a stub pending a later
    /// task).
    #[error("backend unavailable: {backend}")]
    BackendUnavailable {
        /// Human-readable backend label.
        backend: String,
    },

    /// A `file` backend was referenced but `opt_in` was not set to `true`.
    /// This is a config error, not a runtime failure — the user must
    /// explicitly acknowledge that file-based secrets are opt-in (Discipline
    /// §6 — default-on transparency).
    #[error("file secret not opted in: {}", path.display())]
    FileNotOptedIn {
        /// The path that was rejected.
        path: PathBuf,
    },

    /// The named `password-manager` provider plugin is not loaded.
    #[error("secret provider not loaded: {provider}")]
    ProviderNotLoaded {
        /// The provider plugin name.
        provider: String,
    },

    /// `age` decryption failed (wrong identity, corrupted ciphertext, etc.).
    #[error("age decryption failed for {}: {detail}", path.display())]
    Decrypt {
        /// Path to the ciphertext file.
        path: PathBuf,
        /// Description of what went wrong.
        detail: String,
    },

    /// I/O error accessing a file (identity file or ciphertext file).
    #[error("I/O error reading {}: {source}", path.display())]
    Io {
        /// Path that caused the I/O error.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },
}
