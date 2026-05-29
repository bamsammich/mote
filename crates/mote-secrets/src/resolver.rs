//! [`SecretResolver`] — dispatches `resolve(name)` to the right backend.
//!
//! Also defines the [`SecretProviderRouter`] callback trait used by the
//! `password-manager` backend so that `mote-runtime` can implement the
//! targeted `invoke_capability_on` path without creating a dependency cycle.

use std::{collections::BTreeMap, sync::Arc};

use crate::{ResolveError, SecretDef, SecretValue, backend::resolve_backend, def::BackendKind};

/// Routes a `password-manager` secret to a specific named `secret:provider`
/// fulfiller (ADR-0009: explicit, no fan-out — D5). Implemented by
/// `mote-runtime` over `invoke_capability_on`.
///
/// The trait is defined here so `mote-secrets` stays free of any
/// `mote-runtime` dependency (avoiding a cycle).
pub trait SecretProviderRouter: std::fmt::Debug + Send + Sync {
    /// Resolve `reference` against the named `provider` plugin.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::ProviderNotLoaded`] when the plugin is not
    /// active, or another variant when the plugin returns an error.
    fn resolve(&self, provider: &str, reference: &str) -> Result<SecretValue, ResolveError>;
}

/// Resolves named secrets to [`SecretValue`]s by dispatching to the
/// appropriate backend.
///
/// Secret *values* are **never** stored here — only the [`SecretDef`] locators
/// are kept. The value is fetched fresh on every `resolve` call.
#[derive(Debug)]
pub struct SecretResolver {
    defs: BTreeMap<String, SecretDef>,
    /// Router for `password-manager` secrets. `None` until a PM route is
    /// configured (Task 5).
    router: Option<Arc<dyn SecretProviderRouter>>,
}

impl SecretResolver {
    /// Create a new resolver from a set of definitions and an optional PM
    /// router.
    #[must_use]
    pub fn new(
        defs: impl IntoIterator<Item = SecretDef>,
        router: Option<Arc<dyn SecretProviderRouter>>,
    ) -> Self {
        Self {
            defs: defs.into_iter().map(|d| (d.name.clone(), d)).collect(),
            router,
        }
    }

    /// Create a resolver with no definitions (useful as an empty default until
    /// the shell supplies the real config).
    #[must_use]
    pub fn empty() -> Self {
        Self::new(std::iter::empty(), None)
    }

    /// Resolve the named secret to its value.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::NotFound`] when no definition exists for
    /// `name`, or a backend-specific error otherwise.
    pub fn resolve(&self, name: &str) -> Result<SecretValue, ResolveError> {
        let def = self
            .defs
            .get(name)
            .ok_or_else(|| ResolveError::NotFound { name: name.into() })?;

        // Password-manager is special: it delegates to the router.
        if let BackendKind::PasswordManager {
            provider,
            reference,
        } = &def.backend
        {
            return self.router.as_ref().map_or_else(
                || {
                    Err(ResolveError::BackendUnavailable {
                        backend: "password-manager (no router configured)".into(),
                    })
                },
                |r| r.resolve(provider, reference),
            );
        }

        resolve_backend(&def.backend)
    }

    /// The backend label for audit/panel output.
    ///
    /// Returns one of `"keyring"`, `"env"`, `"file"`, `"age"`, or
    /// `"password-manager"`. Returns `None` when no definition exists for
    /// `name`.
    ///
    /// # Note
    ///
    /// For use by the CLI and the integrity panel **only** — never exposed to
    /// plugin Lua.
    #[must_use]
    pub fn backend_label(&self, name: &str) -> Option<&'static str> {
        self.defs.get(name).map(|d| match &d.backend {
            BackendKind::Keyring { .. } => "keyring",
            BackendKind::Env { .. } => "env",
            BackendKind::File { .. } => "file",
            BackendKind::Age { .. } => "age",
            BackendKind::PasswordManager { .. } => "password-manager",
        })
    }

    /// Iterate over secret names for the CLI/panel **only** — never exposed to
    /// plugin Lua.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use age::x25519::Identity;
    use secrecy::ExposeSecret as _;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::def::{BackendKind, SecretDef};

    fn env_def(name: &str, var: &str) -> SecretDef {
        SecretDef {
            name: name.into(),
            backend: BackendKind::Env { var: var.into() },
        }
    }

    // -----------------------------------------------------------------------
    // SecretResolver unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolver_resolves_env_secret() {
        // Use HOME — always set in any Unix process; avoids unsafe set_var.
        let home = std::env::var("HOME").expect("HOME must be set");
        let resolver = SecretResolver::new([env_def("my_key", "HOME")], None);
        let val = resolver.resolve("my_key").expect("should resolve");
        assert_eq!(val.expose_secret(), home.as_str());
    }

    #[test]
    fn resolver_returns_not_found_for_unknown_name() {
        let resolver = SecretResolver::empty();
        let err = resolver.resolve("no_such_secret").expect_err("should fail");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn backend_label_returns_correct_label() {
        // env var value is not read for backend_label — any name works.
        let resolver = SecretResolver::new(
            [
                SecretDef {
                    name: "e".into(),
                    backend: BackendKind::Env { var: "HOME".into() },
                },
                SecretDef {
                    name: "f".into(),
                    backend: BackendKind::File {
                        path: "/tmp/x".into(),
                        opt_in: true,
                    },
                },
                SecretDef {
                    name: "a".into(),
                    backend: BackendKind::Age {
                        path: "/tmp/y.age".into(),
                        identity: None,
                    },
                },
                SecretDef {
                    name: "k".into(),
                    backend: BackendKind::Keyring {
                        id: "svc/acct".into(),
                    },
                },
                SecretDef {
                    name: "p".into(),
                    backend: BackendKind::PasswordManager {
                        provider: "bitwarden".into(),
                        reference: "ref".into(),
                    },
                },
            ],
            None,
        );
        assert_eq!(resolver.backend_label("e"), Some("env"));
        assert_eq!(resolver.backend_label("f"), Some("file"));
        assert_eq!(resolver.backend_label("a"), Some("age"));
        assert_eq!(resolver.backend_label("k"), Some("keyring"));
        assert_eq!(resolver.backend_label("p"), Some("password-manager"));
        assert_eq!(resolver.backend_label("missing"), None);
    }

    #[test]
    fn names_iterator_returns_all_names() {
        let resolver = SecretResolver::new(
            [
                env_def("alpha", "A"),
                env_def("beta", "B"),
                env_def("gamma", "C"),
            ],
            None,
        );
        let mut names: Vec<&str> = resolver.names().collect();
        names.sort_unstable();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn password_manager_without_router_returns_backend_unavailable() {
        let resolver = SecretResolver::new(
            [SecretDef {
                name: "pm".into(),
                backend: BackendKind::PasswordManager {
                    provider: "bw".into(),
                    reference: "vault/item".into(),
                },
            }],
            None,
        );
        let err = resolver.resolve("pm").expect_err("should fail");
        assert!(matches!(err, ResolveError::BackendUnavailable { .. }));
    }

    #[test]
    fn password_manager_with_router_delegates() {
        use std::sync::Mutex;

        /// Fixture router that captures the exact provider/reference args it
        /// receives so the test can assert that arg-threading is correct.
        /// This protects Task 5's real router wiring.
        #[derive(Debug)]
        struct CapturingRouter {
            captured: Mutex<Option<(String, String)>>,
        }
        impl SecretProviderRouter for CapturingRouter {
            fn resolve(
                &self,
                provider: &str,
                reference: &str,
            ) -> Result<SecretValue, ResolveError> {
                *self.captured.lock().expect("lock") =
                    Some((provider.to_owned(), reference.to_owned()));
                Ok(secrecy::SecretString::new("from_router".to_string().into()))
            }
        }

        let router = Arc::new(CapturingRouter {
            captured: Mutex::new(None),
        });
        // Keep a typed clone for reading back the captured args after resolve().
        let router_ref = Arc::clone(&router);
        let router_dyn: Arc<dyn SecretProviderRouter> = router;
        let resolver = SecretResolver::new(
            [SecretDef {
                name: "pm".into(),
                backend: BackendKind::PasswordManager {
                    provider: "bw".into(),
                    reference: "vault/item".into(),
                },
            }],
            Some(router_dyn),
        );
        let val = resolver.resolve("pm").expect("router should supply value");
        assert_eq!(val.expose_secret(), "from_router");

        // Assert the router received exactly the provider + reference from the SecretDef.
        let captured = router_ref
            .captured
            .lock()
            .expect("lock")
            .take()
            .expect("router must have been called");
        assert_eq!(
            captured,
            ("bw".to_owned(), "vault/item".to_owned()),
            "router must receive provider and reference from the SecretDef verbatim"
        );
    }

    // -----------------------------------------------------------------------
    // age integration via resolver
    // -----------------------------------------------------------------------

    #[test]
    fn resolver_resolves_age_secret() {
        use secrecy::ExposeSecret as _;

        let identity = Identity::generate();
        let recipient = identity.to_public();

        let mut ciphertext: Vec<u8> = Vec::new();
        let enc =
            age::Encryptor::with_recipients(std::iter::once::<&dyn age::Recipient>(&recipient))
                .unwrap();
        let mut w = enc.wrap_output(&mut ciphertext).unwrap();
        w.write_all(b"resolver_age_secret").unwrap();
        w.finish().unwrap();

        let mut ct_file = NamedTempFile::new().unwrap();
        ct_file.write_all(&ciphertext).unwrap();
        ct_file.flush().unwrap();

        let id_str = identity.to_string();
        let mut id_file = NamedTempFile::new().unwrap();
        writeln!(id_file, "{}", id_str.expose_secret()).unwrap();
        id_file.flush().unwrap();

        let resolver = SecretResolver::new(
            [SecretDef {
                name: "encrypted_key".into(),
                backend: BackendKind::Age {
                    path: ct_file.path().to_owned(),
                    identity: Some(id_file.path().to_owned()),
                },
            }],
            None,
        );

        let val = resolver.resolve("encrypted_key").expect("should decrypt");
        assert_eq!(val.expose_secret(), "resolver_age_secret");
    }
}
