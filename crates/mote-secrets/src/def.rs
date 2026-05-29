//! Typed secret definitions — `SecretDef` + `BackendKind`.
//!
//! These types carry only *locators* (paths, env-var names, references).
//! They never hold a secret value. The resolved value lives exclusively in
//! [`crate::SecretValue`] (`secrecy::SecretString`), which is zeroized on
//! drop and redacts its `Debug` output.

use std::path::PathBuf;

/// Identifies which backend resolves a named secret and what parameters it
/// needs. Carries only locators — never the secret value itself.
#[derive(Debug, Clone)]
pub enum BackendKind {
    /// OS keyring (Secret Service / macOS Keychain / Windows Credential Store).
    /// `id` is `"service/account"` (split on last `/`).
    /// Resolved by Task 3 — stub here.
    Keyring {
        /// Service/account identifier (`"service"` or `"service/account"`).
        id: String,
    },

    /// Environment variable. The variable must be set in the process
    /// environment at resolution time.
    Env {
        /// Name of the environment variable to read.
        var: String,
    },

    /// Plain-text file on disk. The user must explicitly set `opt_in = true`
    /// in `secrets.lua` — see Discipline §6 (default-on transparency).
    File {
        /// Filesystem path to the secret file.
        path: PathBuf,
        /// Must be `true`; a `false` value (or missing) raises
        /// [`crate::ResolveError::FileNotOptedIn`].
        opt_in: bool,
    },

    /// `age`-encrypted file, decrypted with a native-X25519 identity file.
    /// No passphrase, no SSH keys — D3 from the Phase-4 design doc.
    Age {
        /// Path to the `age`-encrypted ciphertext file.
        path: PathBuf,
        /// Path to the identity (private key) file.
        /// Defaults to `~/.config/mote/secrets/key.txt` when `None`.
        identity: Option<PathBuf>,
    },

    /// Targeted route to a named `secret:provider` fulfiller plugin.
    /// The `provider` field names the specific plugin; resolution is never
    /// broadcast (ADR-0009: explicit, no fan-out — D5).
    /// Resolved by Task 5 — stub here.
    PasswordManager {
        /// Plugin name of the `secret:provider` fulfiller.
        provider: String,
        /// Provider-specific reference (e.g. `"op://Vault/Item/field"`).
        reference: String,
    },
}

/// A fully typed secret declaration produced from a raw `SecretEntry`.
///
/// Carries only a name and a backend locator — never a secret value.
#[derive(Debug, Clone)]
pub struct SecretDef {
    /// The logical name by which the secret is referenced in `secrets.get`.
    pub name: String,
    /// Which backend resolves this secret and with what parameters.
    pub backend: BackendKind,
}
