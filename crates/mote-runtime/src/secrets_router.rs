//! Runtime-side implementation of [`SecretProviderRouter`].
//!
//! [`RuntimeSecretRouter`] bridges the `mote-secrets` callback trait to the
//! runtime's targeted dispatch primitive [`Core::invoke_capability_on`]
//! (ADR-0009: explicit, no fan-out).  The trait is defined in `mote-secrets` so
//! that crate stays free of any `mote-runtime` dependency; this module is the
//! only place that knows about both sides.
//!
//! Bounded scope (ADR-0009): `invoke_capability_on` is called only from here,
//! and only for the `secret:provider` capability.

use std::rc::Rc;

use mote_audit::EventProducer;
use mote_secrets::{ResolveError, SecretProviderRouter, SecretString, SecretValue};
use mote_types::PluginName;

use crate::core::{Core, InvokeOutcome};
use crate::value::HostValue;

/// The caller identity presented to the audit trail for secret-resolution
/// invocations.  A pseudo-name that identifies the secret subsystem as the
/// initiator of the targeted invocation.  Must satisfy [`PluginName`] grammar
/// (`[a-z0-9-]+`); chosen to be recognisable in audit records without
/// colliding with any real plugin name.
const SECRET_SUBSYSTEM_CALLER: &str = "secrets-subsystem";

/// Implements [`SecretProviderRouter`] over [`Core::invoke_capability_on`].
///
/// Holds a [`Core`] handle (cheaply cloneable, `Rc`-backed) and an
/// [`EventProducer`] for audit recording.  Created once per runtime via
/// [`RuntimeSecretRouter::new`] and typically wrapped in an `Rc` for sharing
/// with a [`mote_secrets::SecretResolver`].
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSecretRouter {
    core: Core,
    audit: EventProducer,
}

impl RuntimeSecretRouter {
    /// Build a new router from the runtime's shared core and audit producer.
    pub(crate) const fn new(core: Core, audit: EventProducer) -> Self {
        Self { core, audit }
    }

    /// Wrap `self` in an `Rc<dyn SecretProviderRouter>` for use as a
    /// resolver router.  Uses `Rc` because the runtime core is single-threaded.
    pub(crate) fn into_rc(self) -> Rc<dyn SecretProviderRouter> {
        Rc::new(self)
    }
}

impl SecretProviderRouter for RuntimeSecretRouter {
    /// Invoke `resolve_secret(reference)` on the named `secret:provider`
    /// fulfiller.
    ///
    /// # Errors
    ///
    /// - [`ResolveError::ProviderNotLoaded`] — `provider` is not among the
    ///   registered fulfillers for `secret:provider` (i.e. the plugin is not
    ///   loaded), or it is not a fulfiller of that capability at all.
    /// - [`ResolveError::BackendUnavailable`] — the invocation failed for
    ///   another runtime reason (timeout, Lua error, missing API function).
    fn resolve(&self, provider: &str, reference: &str) -> Result<SecretValue, ResolveError> {
        let caller =
            PluginName::new(SECRET_SUBSYSTEM_CALLER).expect("constant plugin name is valid");
        let provider_name =
            PluginName::new(provider).map_err(|_| ResolveError::ProviderNotLoaded {
                provider: provider.to_owned(),
            })?;

        let arg = HostValue::Str(reference.to_owned());

        match self.core.invoke_capability_on(
            &caller,
            &provider_name,
            "secret:provider",
            "resolve_secret",
            &arg,
            &self.audit,
        ) {
            InvokeOutcome::Ok(HostValue::Str(s)) => Ok(SecretString::new(s.into())),
            InvokeOutcome::Ok(other) => {
                // The fulfiller returned a non-string value.  Map it to a
                // string for usability (best-effort), or return an error.
                Err(ResolveError::BackendUnavailable {
                    backend: format!(
                        "password-manager provider `{provider}` returned \
                         unexpected value type: {other:?}"
                    ),
                })
            }
            InvokeOutcome::NoFulfiller => Err(ResolveError::ProviderNotLoaded {
                provider: provider.to_owned(),
            }),
            InvokeOutcome::NotInContract => Err(ResolveError::BackendUnavailable {
                backend: format!(
                    "resolve_secret is not in the secret:provider contract \
                     (registry inconsistency for provider `{provider}`)"
                ),
            }),
            InvokeOutcome::NoSuchFunction => Err(ResolveError::BackendUnavailable {
                backend: format!("provider `{provider}` has no resolve_secret in M.api"),
            }),
            InvokeOutcome::Timeout => Err(ResolveError::BackendUnavailable {
                backend: format!("provider `{provider}` timed out resolving secret reference"),
            }),
            InvokeOutcome::Failed => Err(ResolveError::BackendUnavailable {
                backend: format!(
                    "provider `{provider}` returned a Lua error resolving secret reference"
                ),
            }),
            InvokeOutcome::Multi { .. } => {
                // invoke_capability_on never returns Multi (it is a targeted
                // single-fulfiller call); guard for exhaustiveness.
                Err(ResolveError::BackendUnavailable {
                    backend: format!(
                        "unexpected Multi outcome from targeted call to provider `{provider}`"
                    ),
                })
            }
        }
    }
}
