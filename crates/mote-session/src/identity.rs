//! The identity axis — who the user is being.
//!
//! An [`Identity`] holds the metadata for a single user identity. The actual
//! Chromium profile (cookies, `localStorage`, cache) lives in `mote-cef`'s
//! `ProfileHandle`; here we track the reference (the on-disk path) and the
//! user-visible name.
//!
//! Identity isolation is described in `docs/identity-isolation.md`. The
//! isolation is **not total** — this crate never claims "fully isolated"; see
//! `DISCIPLINES.md` §5.

use std::path::PathBuf;

use mote_types::IdentityId;
use serde::{Deserialize, Serialize};

use crate::serde_helpers::identity_id as serde_identity_id;

/// Metadata for a single user identity.
///
/// The Chromium profile lives on disk at
/// [`profile_path`](Identity::profile_path); that path is the argument to
/// `mote-cef::ProfileHandle::open`. This crate holds the reference and the
/// user-visible name; it does not open the profile.
///
/// Isolation guarantee: a tab in identity A is isolated from identity B across
/// cookies, `localStorage`/`IndexedDB`, browsing history, and the HTTP disk
/// cache directory — see `docs/identity-isolation.md` for the exact boundary
/// and known leakage surfaces. The isolation is **not** total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Opaque numeric handle — stable across restarts.
    #[serde(with = "serde_identity_id")]
    pub id: IdentityId,
    /// User-visible name (e.g. `"default"`, `"work"`, `"personal"`).
    pub name: String,
    /// Absolute path to the Chromium profile directory for this identity.
    ///
    /// Typically `~/.local/state/mote/<name>/profile/`.
    pub profile_path: PathBuf,
}

impl Identity {
    /// Creates the implicit single "default" identity present on first launch.
    ///
    /// New users have a single default identity and never see the concept; it
    /// only surfaces when a second identity is explicitly created.
    #[must_use]
    pub fn default_identity() -> Self {
        Self {
            id: IdentityId::new(0),
            name: "default".to_owned(),
            // The shell resolves the real path at launch; an empty path here
            // signals "not yet resolved."
            profile_path: PathBuf::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_roundtrip_json() {
        let id = Identity {
            id: IdentityId::new(1),
            name: "work".to_owned(),
            profile_path: PathBuf::from("/home/user/.local/state/mote/work"),
        };
        let json = serde_json::to_string(&id).unwrap();
        let back: Identity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, id.id);
        assert_eq!(back.name, id.name);
        assert_eq!(back.profile_path, id.profile_path);
    }

    #[test]
    fn default_identity_is_named_default() {
        let id = Identity::default_identity();
        assert_eq!(id.name, "default");
        assert_eq!(id.id, IdentityId::new(0));
    }
}
