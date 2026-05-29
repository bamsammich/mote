//! Per-backend resolve implementations.
//!
//! Each backend takes a [`crate::def::BackendKind`] variant and returns a
//! [`crate::SecretValue`] or a [`crate::ResolveError`]. Secret values are
//! **never** logged, displayed, or held in plain `String` — they live
//! exclusively in [`secrecy::SecretString`] (zeroized on drop).

use std::io::Read as _;

use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::{BackendKind, ResolveError, SecretValue};

/// Dispatch to the correct backend based on `kind`.
///
/// # Errors
///
/// Returns a [`ResolveError`] when the secret cannot be resolved by the
/// chosen backend (missing env var, file not opted-in, decryption failure,
/// etc.).
pub(crate) fn resolve_backend(kind: &BackendKind) -> Result<SecretValue, ResolveError> {
    match kind {
        BackendKind::Env { var } => resolve_env(var),
        BackendKind::File { path, opt_in } => resolve_file(path, *opt_in),
        BackendKind::Age { path, identity } => resolve_age(path, identity.as_deref()),
        BackendKind::Keyring { .. } => {
            // Task 3 — keyring backend. Not implemented in this task.
            Err(ResolveError::BackendUnavailable {
                backend: "keyring".into(),
            })
        }
        BackendKind::PasswordManager { .. } => {
            // Task 5 — password-manager targeted-dispatch backend.
            // The router is wired by mote-runtime; here we cannot call it without
            // a reference to the SecretResolver (see SecretResolver::resolve).
            Err(ResolveError::BackendUnavailable {
                backend: "password-manager".into(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// env backend
// ---------------------------------------------------------------------------

/// Read an environment variable and wrap it as a [`SecretValue`].
fn resolve_env(var: &str) -> Result<SecretValue, ResolveError> {
    std::env::var(var)
        .map(|s| SecretString::new(s.into()))
        .map_err(|_| ResolveError::NotFound { name: var.into() })
}

// ---------------------------------------------------------------------------
// file backend
// ---------------------------------------------------------------------------

/// Read a plain-text secret file (must be explicitly opted-in).
///
/// Path expansion (including `~` → home directory) is the CALLER's
/// responsibility (mote-pluginmgr / Task 7). The design doc's examples use
/// `~/…` paths, but this backend passes `path` straight to `std::fs` — it does
/// not expand tildes or env vars.
fn resolve_file(path: &std::path::Path, opt_in: bool) -> Result<SecretValue, ResolveError> {
    if !opt_in {
        return Err(ResolveError::FileNotOptedIn {
            path: path.to_owned(),
        });
    }

    let mut content = std::fs::read_to_string(path).map_err(|e| ResolveError::Io {
        path: path.to_owned(),
        source: e,
    })?;

    // Strip exactly one trailing newline (editors commonly add one).
    if content.ends_with('\n') {
        content.pop();
        if content.ends_with('\r') {
            content.pop();
        }
    }

    Ok(SecretString::new(content.into()))
}

// ---------------------------------------------------------------------------
// age backend
// ---------------------------------------------------------------------------

/// Decrypt an `age`-encrypted file using a native-X25519 identity file.
/// No passphrase, no SSH keys — D3.
fn resolve_age(
    path: &std::path::Path,
    identity_path: Option<&std::path::Path>,
) -> Result<SecretValue, ResolveError> {
    // Resolve the identity file path: explicit param or default location.
    let default_identity = dirs_identity_path();
    let id_path = identity_path.unwrap_or_else(|| {
        default_identity
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(""))
    });

    if id_path.as_os_str().is_empty() {
        return Err(ResolveError::BackendUnavailable {
            backend: "age (cannot determine home directory)".into(),
        });
    }

    // Read the identity file.
    // WHY: id_content holds the raw bech32 private key — must zeroize on drop.
    let id_content: Zeroizing<String> = Zeroizing::new(std::fs::read_to_string(id_path).map_err(
        |e| ResolveError::Io {
            path: id_path.to_owned(),
            source: e,
        },
    )?);

    // Parse the first X25519 identity from the file, skipping comments/blanks.
    let identity = id_content
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                None
            } else {
                Some(line.parse::<age::x25519::Identity>())
            }
        })
        .ok_or_else(|| ResolveError::Decrypt {
            path: id_path.to_owned(),
            detail: "identity file contains no valid X25519 key".into(),
        })?
        .map_err(|e| ResolveError::Decrypt {
            path: id_path.to_owned(),
            detail: e.to_string(),
        })?;

    // Open and decrypt the ciphertext file.
    let ciphertext = std::fs::read(path).map_err(|e| ResolveError::Io {
        path: path.to_owned(),
        source: e,
    })?;

    // `age::Decryptor::new` returns a `Decryptor<R>` struct (not an enum) in
    // age 0.11.x.  `is_scrypt()` would indicate a passphrase file — reject it.
    let decryptor =
        age::Decryptor::new(ciphertext.as_slice()).map_err(|e| ResolveError::Decrypt {
            path: path.to_owned(),
            detail: e.to_string(),
        })?;

    if decryptor.is_scrypt() {
        return Err(ResolveError::Decrypt {
            path: path.to_owned(),
            detail: "passphrase-protected age files are not supported (D3)".into(),
        });
    }

    // WHY: plaintext holds the decrypted secret — must zeroize on drop, including
    // the partial-read-then-error path where SecretString is never constructed.
    let mut plaintext: Zeroizing<String> = Zeroizing::new(String::new());
    decryptor
        .decrypt(std::iter::once::<&dyn age::Identity>(&identity))
        .map_err(|e| ResolveError::Decrypt {
            path: path.to_owned(),
            detail: e.to_string(),
        })?
        .read_to_string(&mut plaintext)
        .map_err(|e| ResolveError::Io {
            path: path.to_owned(),
            source: e,
        })?;

    // Move the inner String out so only SecretString owns (and zeroizes) it.
    Ok(SecretString::new(std::mem::take(&mut *plaintext).into()))
}

/// Returns the default `age` identity path: `~/.config/mote/secrets/key.txt`.
///
/// Uses `$XDG_CONFIG_HOME` when set, otherwise `$HOME/.config`.
fn dirs_identity_path() -> Option<std::path::PathBuf> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(config_dir.join("mote").join("secrets").join("key.txt"))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use age::x25519::Identity;
    use secrecy::ExposeSecret as _;
    use tempfile::NamedTempFile;

    use super::*;

    // -----------------------------------------------------------------------
    // env backend tests
    // -----------------------------------------------------------------------

    #[test]
    fn env_resolves_set_variable() {
        // Use a variable that is always present in any Unix process.
        // This avoids set_var, which is unsafe in multithreaded contexts.
        let home = std::env::var("HOME").expect("HOME must be set in test environment");
        let kind = BackendKind::Env { var: "HOME".into() };
        let val = resolve_backend(&kind).expect("should resolve");
        assert_eq!(val.expose_secret(), home.as_str());
    }

    #[test]
    fn env_returns_not_found_for_unset_variable() {
        // This name is guaranteed absent: it is never set by any test fixture
        // or shell, and we do not use remove_var (unsafe).
        let kind = BackendKind::Env {
            var: "MOTE_TEST_SECRET_NEVER_SET_XZXZXZ_1234567890".into(),
        };
        let err = resolve_backend(&kind).expect_err("should fail");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    // -----------------------------------------------------------------------
    // file backend tests
    // -----------------------------------------------------------------------

    #[test]
    fn file_resolves_opted_in_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "mysecretvalue").unwrap();
        let kind = BackendKind::File {
            path: tmp.path().to_owned(),
            opt_in: true,
        };
        let val = resolve_backend(&kind).expect("should resolve");
        // Trailing newline must be stripped.
        assert_eq!(val.expose_secret(), "mysecretvalue");
    }

    #[test]
    fn file_errors_when_not_opted_in() {
        let tmp = NamedTempFile::new().unwrap();
        let kind = BackendKind::File {
            path: tmp.path().to_owned(),
            opt_in: false,
        };
        let err = resolve_backend(&kind).expect_err("should fail");
        assert!(matches!(err, ResolveError::FileNotOptedIn { .. }));
    }

    #[test]
    fn file_errors_on_missing_file() {
        let kind = BackendKind::File {
            path: std::path::PathBuf::from("/tmp/mote_test_file_does_not_exist_xyz.secret"),
            opt_in: true,
        };
        let err = resolve_backend(&kind).expect_err("should fail");
        assert!(matches!(err, ResolveError::Io { .. }));
    }

    // -----------------------------------------------------------------------
    // age backend tests
    // -----------------------------------------------------------------------

    /// Helper: generate an X25519 identity and encrypt `plaintext` with the
    /// corresponding recipient. Returns `(identity, ciphertext_file)`.
    fn generate_age_fixture(plaintext: &str) -> (Identity, NamedTempFile) {
        let identity = Identity::generate();
        let recipient = identity.to_public();

        let mut ciphertext: Vec<u8> = Vec::new();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once::<&dyn age::Recipient>(&recipient))
                .expect("encryptor creation");
        let mut writer = encryptor.wrap_output(&mut ciphertext).expect("wrap_output");
        writer.write_all(plaintext.as_bytes()).unwrap();
        writer.finish().unwrap();

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&ciphertext).unwrap();
        tmp.flush().unwrap();

        (identity, tmp)
    }

    /// Helper: write an identity (private key) to a temp file.
    fn write_identity_file(identity: &Identity) -> NamedTempFile {
        use secrecy::ExposeSecret as _;
        let secret_str = identity.to_string();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", secret_str.expose_secret()).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn age_decrypts_with_correct_identity() {
        let (identity, ciphertext_file) = generate_age_fixture("age_secret_42");
        let id_file = write_identity_file(&identity);

        let kind = BackendKind::Age {
            path: ciphertext_file.path().to_owned(),
            identity: Some(id_file.path().to_owned()),
        };
        let val = resolve_backend(&kind).expect("should decrypt");
        assert_eq!(val.expose_secret(), "age_secret_42");
    }

    #[test]
    fn age_errors_with_wrong_identity() {
        let (_enc_identity, ciphertext_file) = generate_age_fixture("secret");
        // Generate a completely different identity — decryption must fail.
        let wrong_identity = Identity::generate();
        let id_file = write_identity_file(&wrong_identity);

        let kind = BackendKind::Age {
            path: ciphertext_file.path().to_owned(),
            identity: Some(id_file.path().to_owned()),
        };
        let err = resolve_backend(&kind).expect_err("should fail with wrong identity");
        assert!(matches!(err, ResolveError::Decrypt { .. }));
    }

    #[test]
    fn age_errors_with_missing_identity_file() {
        let (_identity, ciphertext_file) = generate_age_fixture("secret");
        let kind = BackendKind::Age {
            path: ciphertext_file.path().to_owned(),
            identity: Some(std::path::PathBuf::from(
                "/tmp/mote_test_age_identity_does_not_exist_xyz.txt",
            )),
        };
        let err = resolve_backend(&kind).expect_err("should fail");
        assert!(matches!(err, ResolveError::Io { .. }));
    }

    // -----------------------------------------------------------------------
    // stub backend tests
    // -----------------------------------------------------------------------

    #[test]
    fn keyring_returns_backend_unavailable() {
        let kind = BackendKind::Keyring {
            id: "svc/acct".into(),
        };
        let err = resolve_backend(&kind).expect_err("keyring is a stub");
        assert!(matches!(err, ResolveError::BackendUnavailable { .. }));
    }

    #[test]
    fn password_manager_returns_backend_unavailable() {
        let kind = BackendKind::PasswordManager {
            provider: "1password".into(),
            reference: "op://Vault/Item/field".into(),
        };
        let err = resolve_backend(&kind).expect_err("PM is a stub");
        assert!(matches!(err, ResolveError::BackendUnavailable { .. }));
    }
}
