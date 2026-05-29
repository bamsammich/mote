//! Secrets composition and conversion for `mote-pluginmgr`.
//!
//! This module owns two things:
//!
//! 1. **`composed_secrets_config`** — reads `<config>/secrets.lua` and the
//!    per-identity overlay `<config>/identities/<id>/secrets.lua`, applies
//!    last-name-wins semantics, and returns a flat `Vec<SecretEntry>`.
//!    Mirrors the logic of
//!    [`PluginManager::composed_config`][crate::manager::PluginManager::composed_config].
//!
//! 2. **`convert_secret`** — validates a single raw [`mote_lua::SecretEntry`]
//!    against the expected fields for its backend and produces a
//!    [`mote_secrets::SecretDef`].  Missing required fields or unknown backends
//!    surface as a typed [`SecretConvertError`] that names the offending secret.
//!    `~`/`$HOME` expansion in `file` and `age` paths is performed here (this
//!    layer's job per the brief — `mote-secrets` passes paths straight to
//!    `std::fs`).
//!
//! Callers that want a fully-resolved [`mote_secrets::SecretResolver`] should
//! call [`build_secret_resolver`], which composes and converts all entries and
//! wires up the optional PM router.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use mote_lua::{SecretEntry, SecretParam};
use mote_secrets::{BackendKind, SecretDef, SecretProviderRouter, SecretResolver};
use mote_types::IdentityId;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// An error that arises while validating a raw [`SecretEntry`] during
/// conversion to a typed [`SecretDef`].
///
/// Every variant carries the name of the offending secret so errors are
/// actionable.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecretConvertError {
    /// The `backend` field names a backend this version of Mote does not
    /// recognise.
    #[error("secret `{name}`: unknown backend `{backend}`")]
    UnknownBackend {
        /// Name of the offending secret.
        name: String,
        /// The unrecognised backend string.
        backend: String,
    },

    /// A required parameter field is absent from the secret entry.
    #[error("secret `{name}`: missing required parameter `{field}` for backend `{backend}`")]
    MissingField {
        /// Name of the offending secret.
        name: String,
        /// The parameter that was absent.
        field: &'static str,
        /// The backend that requires it.
        backend: &'static str,
    },

    /// A `file` backend entry was present but `opt_in` was not set to `true`.
    ///
    /// The user must explicitly set `opt_in = true` in `secrets.lua` to
    /// acknowledge that file-based secrets are opt-in (Discipline §6).
    #[error("secret `{name}`: file backend requires `opt_in = true` (missing or false)")]
    FileNotOptedIn {
        /// Name of the offending secret.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compose the raw secret entries for `identity` from the config-file layers.
///
/// ## Layers (applied in order, last-wins per name)
///
/// 1. Global `<config>/secrets.lua` — if present.
/// 2. Per-identity `<config>/identities/<id>/secrets.lua` — if `identity` is
///    `Some` **and** the file exists.
///
/// Missing files are silently skipped (no error).  The returned entries are
/// deduplicated by name with the overlay winning, mirroring
/// [`PluginManager::composed_config`].
///
/// # Errors
///
/// Returns [`crate::manager::ManagerError`] only on I/O or Lua parse failure.
/// Unknown-backend / missing-field errors surface later in [`convert_secret`].
pub fn composed_secrets_config(
    config_dir: &Path,
    identity: Option<&IdentityId>,
) -> Result<Vec<SecretEntry>, crate::manager::ManagerError> {
    let global_path = config_dir.join("secrets.lua");
    let identity_path = identity.map(|id| {
        config_dir
            .join("identities")
            .join(id.to_string())
            .join("secrets.lua")
    });

    let global_entries = if global_path.exists() {
        let src = std::fs::read_to_string(&global_path).map_err(|e| {
            crate::manager::ManagerError::Io {
                path: global_path.clone(),
                source: e,
            }
        })?;
        let spec = mote_lua::eval_config(&src, "secrets.lua")?;
        spec.secrets
    } else {
        Vec::new()
    };

    let identity_entries = if let Some(p) = identity_path.as_ref().filter(|p| p.exists()) {
        let src = std::fs::read_to_string(p).map_err(|e| crate::manager::ManagerError::Io {
            path: p.clone(),
            source: e,
        })?;
        let spec = mote_lua::eval_config(&src, "identities/<id>/secrets.lua")?;
        spec.secrets
    } else {
        Vec::new()
    };

    // Merge: start with global, then apply overlay with last-name-wins.
    let mut merged: Vec<SecretEntry> = global_entries;
    for entry in identity_entries {
        if let Some(existing) = merged.iter_mut().find(|e| e.name == entry.name) {
            *existing = entry;
        } else {
            merged.push(entry);
        }
    }

    Ok(merged)
}

/// Convert a single raw [`SecretEntry`] (from `mote-lua`) to a typed
/// [`SecretDef`] (for `mote-secrets`).
///
/// Per-backend validation:
/// - `keyring` — requires `id`.
/// - `env` — requires `var`.
/// - `file` — requires `path` **and** `opt_in = true`; absence or `false` is
///   rejected.
/// - `age` — requires `path`; `identity` is optional.
/// - `password-manager` — requires both `provider` and `reference` (ADR-0009).
///
/// `~` / `$HOME` expansion is applied to `path` and `identity` fields for the
/// `file` and `age` backends here — `mote-secrets` passes paths straight to
/// `std::fs` and does not expand tildes.
///
/// # Errors
///
/// Returns a [`SecretConvertError`] that names the offending secret when a
/// required field is missing, the backend is unknown, or a `file` entry lacks
/// `opt_in = true`.
pub fn convert_secret(entry: &SecretEntry) -> Result<SecretDef, SecretConvertError> {
    match entry.backend.as_str() {
        "keyring" => {
            let id = require_str(entry, "id", "keyring")?;
            Ok(SecretDef {
                name: entry.name.clone(),
                backend: BackendKind::Keyring { id },
            })
        }

        "env" => {
            let var = require_str(entry, "var", "env")?;
            Ok(SecretDef {
                name: entry.name.clone(),
                backend: BackendKind::Env { var },
            })
        }

        "file" => {
            let raw_path = require_str(entry, "path", "file")?;
            // opt_in must be explicitly Bool(true).
            let opt_in = matches!(entry.params.get("opt_in"), Some(SecretParam::Bool(true)));
            if !opt_in {
                return Err(SecretConvertError::FileNotOptedIn {
                    name: entry.name.clone(),
                });
            }
            let path = expand_tilde(Path::new(&raw_path));
            Ok(SecretDef {
                name: entry.name.clone(),
                backend: BackendKind::File { path, opt_in: true },
            })
        }

        "age" => {
            let raw_path = require_str(entry, "path", "age")?;
            let path = expand_tilde(Path::new(&raw_path));
            let identity = entry.params.get("identity").and_then(|p| {
                if let SecretParam::Str(s) = p {
                    Some(expand_tilde(Path::new(s)))
                } else {
                    None
                }
            });
            Ok(SecretDef {
                name: entry.name.clone(),
                backend: BackendKind::Age { path, identity },
            })
        }

        "password-manager" => {
            let provider = require_str(entry, "provider", "password-manager")?;
            let reference = require_str(entry, "reference", "password-manager")?;
            Ok(SecretDef {
                name: entry.name.clone(),
                backend: BackendKind::PasswordManager {
                    provider,
                    reference,
                },
            })
        }

        other => Err(SecretConvertError::UnknownBackend {
            name: entry.name.clone(),
            backend: other.to_owned(),
        }),
    }
}

/// Build a [`SecretResolver`] from all entries in `config_dir` for `identity`,
/// converting each raw entry to a typed [`SecretDef`].
///
/// Conversion errors are collected; non-fatal by design (the caller receives
/// a resolver over all successfully-converted entries plus the error list).
///
/// # Errors
///
/// Returns [`crate::manager::ManagerError`] only on I/O or Lua parse failure;
/// per-secret conversion errors go into the second element of the returned
/// tuple.
pub fn build_secret_resolver(
    config_dir: &Path,
    identity: Option<&IdentityId>,
    router: Option<Rc<dyn SecretProviderRouter>>,
) -> Result<(SecretResolver, Vec<SecretConvertError>), crate::manager::ManagerError> {
    let entries = composed_secrets_config(config_dir, identity)?;

    let mut defs = Vec::with_capacity(entries.len());
    let mut errors: Vec<SecretConvertError> = Vec::new();

    for entry in &entries {
        match convert_secret(entry) {
            Ok(def) => defs.push(def),
            Err(e) => errors.push(e),
        }
    }

    Ok((SecretResolver::new(defs, router), errors))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Pull a `Str` parameter by name, or return a [`SecretConvertError::MissingField`].
fn require_str(
    entry: &SecretEntry,
    field: &'static str,
    backend: &'static str,
) -> Result<String, SecretConvertError> {
    match entry.params.get(field) {
        Some(SecretParam::Str(s)) => Ok(s.clone()),
        _ => Err(SecretConvertError::MissingField {
            name: entry.name.clone(),
            field,
            backend,
        }),
    }
}

/// Expand a leading `~` to `$HOME`. Mirrors
/// [`PluginManager::expand_tilde`][crate::manager::PluginManager] so both
/// use the same `HOME` var, keeping `~` semantics consistent.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    // bare `~` alone
    if s == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    path.to_path_buf()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use mote_lua::SecretParam;
    use mote_types::IdentityId;
    use secrecy::ExposeSecret as _;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_entry(name: &str, backend: &str, params: &[(&str, SecretParam)]) -> SecretEntry {
        SecretEntry {
            name: name.to_owned(),
            backend: backend.to_owned(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn str_param(s: &str) -> SecretParam {
        SecretParam::Str(s.to_owned())
    }

    fn bool_param(b: bool) -> SecretParam {
        SecretParam::Bool(b)
    }

    // -----------------------------------------------------------------------
    // keyring backend
    // -----------------------------------------------------------------------

    #[test]
    fn keyring_happy_path() {
        let entry = make_entry("my_secret", "keyring", &[("id", str_param("svc/acct"))]);
        let def = convert_secret(&entry).expect("should convert");
        assert_eq!(def.name, "my_secret");
        assert!(matches!(def.backend, BackendKind::Keyring { id } if id == "svc/acct"));
    }

    #[test]
    fn keyring_missing_id_names_the_secret() {
        let entry = make_entry("my_secret", "keyring", &[]);
        let err = convert_secret(&entry).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("my_secret"),
            "error must name the secret: {msg}"
        );
        assert!(
            matches!(err, SecretConvertError::MissingField { field: "id", .. }),
            "wrong variant: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // env backend
    // -----------------------------------------------------------------------

    #[test]
    fn env_happy_path() {
        let entry = make_entry("api_key", "env", &[("var", str_param("MY_API_KEY"))]);
        let def = convert_secret(&entry).expect("should convert");
        assert_eq!(def.name, "api_key");
        assert!(matches!(def.backend, BackendKind::Env { var } if var == "MY_API_KEY"));
    }

    #[test]
    fn env_missing_var_names_the_secret() {
        let entry = make_entry("api_key", "env", &[]);
        let err = convert_secret(&entry).expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("api_key"), "error must name the secret: {msg}");
        assert!(
            matches!(err, SecretConvertError::MissingField { field: "var", .. }),
            "wrong variant: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // file backend
    // -----------------------------------------------------------------------

    #[test]
    fn file_happy_path() {
        let entry = make_entry(
            "cert",
            "file",
            &[
                ("path", str_param("/tmp/cert.pem")),
                ("opt_in", bool_param(true)),
            ],
        );
        let def = convert_secret(&entry).expect("should convert");
        assert_eq!(def.name, "cert");
        assert!(
            matches!(def.backend, BackendKind::File { ref path, opt_in: true } if path == Path::new("/tmp/cert.pem"))
        );
    }

    #[test]
    fn file_without_opt_in_rejected() {
        let entry = make_entry("cert", "file", &[("path", str_param("/tmp/cert.pem"))]);
        let err = convert_secret(&entry).expect_err("should fail — opt_in missing");
        assert!(
            matches!(err, SecretConvertError::FileNotOptedIn { ref name } if name == "cert"),
            "wrong variant: {err:?}"
        );
        assert!(err.to_string().contains("cert"));
    }

    #[test]
    fn file_with_opt_in_false_rejected() {
        let entry = make_entry(
            "cert",
            "file",
            &[
                ("path", str_param("/tmp/cert.pem")),
                ("opt_in", bool_param(false)),
            ],
        );
        let err = convert_secret(&entry).expect_err("should fail — opt_in = false");
        assert!(matches!(err, SecretConvertError::FileNotOptedIn { .. }));
    }

    #[test]
    fn file_missing_path_names_the_secret() {
        let entry = make_entry("cert", "file", &[("opt_in", bool_param(true))]);
        let err = convert_secret(&entry).expect_err("should fail — path missing");
        let msg = err.to_string();
        assert!(msg.contains("cert"), "error must name the secret: {msg}");
        assert!(matches!(
            err,
            SecretConvertError::MissingField { field: "path", .. }
        ));
    }

    // -----------------------------------------------------------------------
    // age backend
    // -----------------------------------------------------------------------

    #[test]
    fn age_happy_path_no_identity() {
        let entry = make_entry(
            "encrypted_key",
            "age",
            &[("path", str_param("/tmp/key.age"))],
        );
        let def = convert_secret(&entry).expect("should convert");
        assert_eq!(def.name, "encrypted_key");
        assert!(
            matches!(def.backend, BackendKind::Age { ref path, identity: None } if path == Path::new("/tmp/key.age"))
        );
    }

    #[test]
    fn age_happy_path_with_identity() {
        let entry = make_entry(
            "encrypted_key",
            "age",
            &[
                ("path", str_param("/tmp/key.age")),
                ("identity", str_param("/tmp/id.txt")),
            ],
        );
        let def = convert_secret(&entry).expect("should convert");
        assert!(
            matches!(def.backend, BackendKind::Age { ref identity, .. } if identity.as_deref() == Some(Path::new("/tmp/id.txt")))
        );
    }

    #[test]
    fn age_missing_path_names_the_secret() {
        let entry = make_entry("encrypted_key", "age", &[]);
        let err = convert_secret(&entry).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("encrypted_key"),
            "error must name the secret: {msg}"
        );
        assert!(matches!(
            err,
            SecretConvertError::MissingField { field: "path", .. }
        ));
    }

    // -----------------------------------------------------------------------
    // password-manager backend
    // -----------------------------------------------------------------------

    #[test]
    fn pm_happy_path() {
        let entry = make_entry(
            "vault_secret",
            "password-manager",
            &[
                ("provider", str_param("bitwarden")),
                ("reference", str_param("op://Vault/Item/field")),
            ],
        );
        let def = convert_secret(&entry).expect("should convert");
        assert_eq!(def.name, "vault_secret");
        assert!(matches!(
            def.backend,
            BackendKind::PasswordManager { ref provider, ref reference }
            if provider == "bitwarden" && reference == "op://Vault/Item/field"
        ));
    }

    #[test]
    fn pm_missing_provider_names_the_secret() {
        let entry = make_entry(
            "vault_secret",
            "password-manager",
            &[("reference", str_param("op://Vault/Item/field"))],
        );
        let err = convert_secret(&entry).expect_err("should fail — provider missing");
        let msg = err.to_string();
        assert!(
            msg.contains("vault_secret"),
            "error must name the secret: {msg}"
        );
        assert!(matches!(
            err,
            SecretConvertError::MissingField {
                field: "provider",
                ..
            }
        ));
    }

    #[test]
    fn pm_missing_reference_names_the_secret() {
        let entry = make_entry(
            "vault_secret",
            "password-manager",
            &[("provider", str_param("bitwarden"))],
        );
        let err = convert_secret(&entry).expect_err("should fail — reference missing");
        let msg = err.to_string();
        assert!(
            msg.contains("vault_secret"),
            "error must name the secret: {msg}"
        );
        assert!(matches!(
            err,
            SecretConvertError::MissingField {
                field: "reference",
                ..
            }
        ));
    }

    // -----------------------------------------------------------------------
    // Unknown backend
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_backend_names_the_secret() {
        let entry = make_entry("my_secret", "telepathy", &[]);
        let err = convert_secret(&entry).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("my_secret"),
            "error must name the secret: {msg}"
        );
        assert!(
            matches!(err, SecretConvertError::UnknownBackend { ref backend, .. } if backend == "telepathy"),
            "wrong variant: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Tilde expansion
    // -----------------------------------------------------------------------

    #[test]
    fn file_tilde_expanded_in_path() {
        let home = std::env::var_os("HOME").expect("HOME must be set");
        let entry = make_entry(
            "tilde_secret",
            "file",
            &[
                ("path", str_param("~/secrets/cert.pem")),
                ("opt_in", bool_param(true)),
            ],
        );
        let def = convert_secret(&entry).expect("should convert");
        let expected = PathBuf::from(&home).join("secrets/cert.pem");
        assert!(
            matches!(def.backend, BackendKind::File { ref path, .. } if *path == expected),
            "tilde not expanded: {def:?}"
        );
    }

    #[test]
    fn age_tilde_expanded_in_path_and_identity() {
        let home = std::env::var_os("HOME").expect("HOME must be set");
        let entry = make_entry(
            "enc_key",
            "age",
            &[
                ("path", str_param("~/enc/secret.age")),
                ("identity", str_param("~/enc/id.txt")),
            ],
        );
        let def = convert_secret(&entry).expect("should convert");
        let expected_path = PathBuf::from(&home).join("enc/secret.age");
        let expected_id = PathBuf::from(&home).join("enc/id.txt");
        assert!(
            matches!(def.backend, BackendKind::Age { ref path, ref identity }
                if *path == expected_path && identity.as_deref() == Some(expected_id.as_path())),
            "tilde not expanded: {def:?}"
        );
    }

    // -----------------------------------------------------------------------
    // composed_secrets_config: global + identity overlay, last-wins
    // -----------------------------------------------------------------------

    #[test]
    fn composed_global_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path();

        // Write a secrets.lua with two entries.
        let lua = r#"
mote.secrets.define({
    api_key = { backend = "env", var = "API_KEY" },
    cert    = { backend = "env", var = "CERT" },
})
"#;
        std::fs::write(config_dir.join("secrets.lua"), lua).expect("write");

        let entries = composed_secrets_config(config_dir, None).expect("compose");
        assert_eq!(entries.len(), 2);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"api_key"), "api_key missing: {names:?}");
        assert!(names.contains(&"cert"), "cert missing: {names:?}");
    }

    #[test]
    fn composed_no_files_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entries = composed_secrets_config(dir.path(), None).expect("compose");
        assert!(entries.is_empty());
    }

    #[test]
    fn composed_identity_overlay_last_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path();

        // Global: two secrets.
        let global_lua = r#"
mote.secrets.define({
    shared = { backend = "env", var = "GLOBAL_SHARED" },
    only_global = { backend = "env", var = "ONLY_GLOBAL" },
})
"#;
        std::fs::write(config_dir.join("secrets.lua"), global_lua).expect("write global");

        // Identity overlay: overrides `shared`, adds `only_identity`.
        let id = IdentityId::new(42_u64);
        let identity_dir = config_dir.join("identities").join(id.to_string());
        std::fs::create_dir_all(&identity_dir).expect("create identity dir");
        let overlay_lua = r#"
mote.secrets.define({
    shared       = { backend = "env", var = "OVERLAY_SHARED" },
    only_identity = { backend = "env", var = "ONLY_IDENTITY" },
})
"#;
        std::fs::write(identity_dir.join("secrets.lua"), overlay_lua).expect("write overlay");

        let entries = composed_secrets_config(config_dir, Some(&id)).expect("compose");

        // Should have 3 entries total (shared overridden, both only_* preserved).
        assert_eq!(entries.len(), 3, "expected 3 entries: {entries:?}");

        let shared = entries
            .iter()
            .find(|e| e.name == "shared")
            .expect("shared missing");
        // The overlay must win for `shared`.
        let var = shared.params.get("var").and_then(|p| {
            if let mote_lua::SecretParam::Str(s) = p {
                Some(s.as_str())
            } else {
                None
            }
        });
        assert_eq!(var, Some("OVERLAY_SHARED"), "overlay must win");

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"only_global"),
            "only_global must be preserved"
        );
        assert!(
            names.contains(&"only_identity"),
            "only_identity must appear"
        );
    }

    #[test]
    fn composed_plugins_less_secrets_file_works() {
        // A secrets.lua that declares secrets but no plugins must be parseable.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path();

        let lua = r#"
-- no mote.plugins() call at all
mote.secrets.define({
    db_pass = { backend = "env", var = "DB_PASSWORD" },
})
"#;
        std::fs::write(config_dir.join("secrets.lua"), lua).expect("write");

        let entries =
            composed_secrets_config(config_dir, None).expect("should parse without plugins");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "db_pass");
    }

    // -----------------------------------------------------------------------
    // build_secret_resolver: end-to-end env resolution
    // -----------------------------------------------------------------------

    #[test]
    fn build_resolver_resolves_env_secret_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path();

        // Use HOME — always set, avoids unsafe set_var.
        let home = std::env::var("HOME").expect("HOME must be set");
        let lua = r#"
mote.secrets.define({
    home_secret = { backend = "env", var = "HOME" },
})
"#;
        std::fs::write(config_dir.join("secrets.lua"), lua).expect("write");

        let (resolver, errors) =
            build_secret_resolver(config_dir, None, None).expect("build resolver");
        assert!(
            errors.is_empty(),
            "unexpected conversion errors: {errors:?}"
        );

        let val = resolver.resolve("home_secret").expect("should resolve");
        assert_eq!(val.expose_secret(), home.as_str());
    }
}
