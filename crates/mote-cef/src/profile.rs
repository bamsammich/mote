//! Per-identity browsing profiles — a Mote identity *is* a Chromium profile.
//!
//! A [`ProfileHandle`] wraps a CEF `RequestContext` configured with a
//! per-identity on-disk storage path. Creating a [`crate::Page`] under a profile
//! routes that page's cookies, localStorage/IndexedDB, history, HTTP disk cache,
//! and site permissions through the profile's `RequestContext`, so two identities
//! do not share that state. See `docs/identity-isolation.md` for the exact
//! isolation surface — what a profile isolates and what it does NOT fully isolate
//! (DISCIPLINES.md §5: "isolated across [enumerated list]", never "fully
//! isolated").
//!
//! # Identity → `RequestContext` mapping
//!
//! - Each identity id maps to exactly one `RequestContext`.
//! - That context's `cache_path` is `<root>/profile-<identity-id>` on disk, so
//!   the per-identity HTTP cache, cookies, and storage live in a distinct
//!   directory.
//! - A [`ProfileManager`] interns contexts by identity id so repeated lookups for
//!   the same identity return the same underlying context (idempotent get/create).
//!
//! # CEF constraint: profile dir MUST be a *direct child* of the engine cache path
//!
//! CEF 148 runs the Chromium **Chrome runtime**, where a `RequestContext`'s
//! `cache_path` becomes a Chrome *profile directory*. Chromium requires each
//! profile directory to be a **direct child** of the global `root_cache_path`
//! configured in [`crate::EngineConfig::cache_path`] (a sibling of CEF's own
//! `Default`, `ShaderCache`, … dirs). Two failure modes were observed and must be
//! avoided:
//!
//! - A `cache_path` that is **not under** `root_cache_path` →
//!   `cache_path is invalid` and CEF **silently falls back to in-memory storage**
//!   (no on-disk isolation).
//! - A `cache_path` **nested more than one level** under `root_cache_path`, or one
//!   **pre-created** by us → `Cannot create profile at path …` and the profile
//!   fails to materialise.
//!
//! Therefore: the [`ProfileManager`] root MUST be exactly the engine's
//! `cache_path`; each identity lives at `<cache_path>/profile-<id>` (one level
//! deep, `profile-` prefixed to avoid colliding with CEF's own subdirs); and the
//! directory is **created by CEF**, never by `mote-cef`. This is a hard
//! requirement, not advice.
//!
//! Like [`crate::Page`], a profile is bound to the CEF UI thread (the thread that
//! pumps the engine); the handle is intentionally not `Send`/`Sync`.
#![allow(
    unsafe_code,
    reason = "request_context_create_context is CEF FFI; contained per DISCIPLINES.md §1"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cef::{CefString, RequestContext, RequestContextSettings, request_context_create_context};

use crate::error::{CefError, Result};

/// A stable, filesystem-safe identifier for an identity.
///
/// Mote identity ids are already constrained to a safe character set upstream;
/// this newtype documents the contract at the `mote-cef` boundary and is
/// validated in [`Self::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdentityId(String);

impl IdentityId {
    /// Wrap an identity id, rejecting values that are empty or contain path
    /// separators / `..` traversal so they cannot escape the profile root.
    ///
    /// # Errors
    /// [`CefError::Profile`] if the id is empty or contains `/`, `\`, or `..`.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        let unsafe_segment = id.is_empty()
            || id == "."
            || id == ".."
            || id.contains('/')
            || id.contains('\\')
            || id.contains("..")
            || id.contains('\0');
        if unsafe_segment {
            return Err(CefError::Profile(
                "identity id must be non-empty and free of path separators / traversal",
            ));
        }
        Ok(Self(id))
    }

    /// The borrowed identity id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdentityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A live Chromium profile for one Mote identity.
///
/// Holds the identity's `RequestContext` plus the on-disk path it isolates state
/// into. Clone is cheap: the inner context is reference-counted by CEF and the
/// path is shared, so clones refer to the *same* profile (not a copy).
#[derive(Clone)]
pub struct ProfileHandle {
    inner: Rc<ProfileInner>,
}

struct ProfileInner {
    id: IdentityId,
    storage_path: PathBuf,
    /// The CEF request context. `RefCell` because CEF's create/use APIs take
    /// `&mut RequestContext`, but a `ProfileHandle` is shared (`Rc`) and only ever
    /// touched on the single CEF UI thread — there is no cross-thread aliasing.
    context: RefCell<RequestContext>,
}

impl std::fmt::Debug for ProfileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileHandle")
            .field("identity", &self.inner.id)
            .field("storage_path", &self.inner.storage_path)
            .finish_non_exhaustive()
    }
}

impl ProfileHandle {
    /// Construct a profile for `id`, isolating its storage under
    /// `<root>/profile-<identity-id>`. `root` MUST be the engine's
    /// `cache_path` (see module docs). Requires a live [`crate::Engine`] (CEF must
    /// be initialised before a request context can be created).
    ///
    /// Prefer [`ProfileManager::get_or_create`] in application code so the same
    /// identity always maps to one context; this constructor is the primitive it
    /// builds on and is exposed for callers managing their own profile set.
    ///
    /// # Errors
    /// [`CefError::Profile`] if CEF could not create the request context.
    pub fn create(id: &IdentityId, root: &Path) -> Result<Self> {
        // Under CEF's Chrome runtime a RequestContext cache_path is a Chrome
        // *profile directory*, which must be a DIRECT child of root_cache_path
        // (siblings of CEF's own `Default`, `ShaderCache`, etc.). We therefore
        // place each identity at `<root>/profile-<id>` and must NOT pre-create
        // the directory: CEF's profile manager creates and owns it. Pre-creating
        // it, or nesting it deeper, makes CEF reject the path and silently fall
        // back to in-memory storage (no on-disk isolation). See module docs.
        let storage_path = root.join(format!("profile-{}", id.as_str()));

        let settings = RequestContextSettings {
            cache_path: CefString::from(&*storage_path.to_string_lossy()),
            // Persist session cookies so identity auth survives a restart, matching
            // the per-identity persistence model (session state is durable per
            // identity). A future "ephemeral identity" mode would flip this.
            persist_session_cookies: 1,
            ..Default::default()
        };

        let context = request_context_create_context(Some(&settings), None)
            .ok_or(CefError::Profile("CEF failed to create request context"))?;

        Ok(Self {
            inner: Rc::new(ProfileInner {
                id: id.clone(),
                storage_path,
                context: RefCell::new(context),
            }),
        })
    }

    /// The identity this profile belongs to.
    #[must_use]
    pub fn identity(&self) -> &IdentityId {
        &self.inner.id
    }

    /// The on-disk directory this profile isolates its state into.
    #[must_use]
    pub fn storage_path(&self) -> &Path {
        &self.inner.storage_path
    }

    /// Run `f` with a mutable borrow of the underlying request context.
    ///
    /// Kept `pub(crate)` so the `cef::RequestContext` never escapes the crate
    /// (DISCIPLINES.md §1); [`crate::Page`] uses this to pass the context to
    /// `browser_host_create_browser_sync`.
    pub(crate) fn with_context<R>(&self, f: impl FnOnce(&mut RequestContext) -> R) -> R {
        f(&mut self.inner.context.borrow_mut())
    }
}

/// Interns one [`ProfileHandle`] per identity id.
///
/// Application code holds a single `ProfileManager` and asks it for a profile by
/// identity id; the manager guarantees that the same identity always resolves to
/// the same underlying `RequestContext` (so two tabs in the same identity share
/// cookies/storage, while different identities do not).
///
/// Not `Send`/`Sync`: lives on the CEF UI thread with the profiles it owns.
pub struct ProfileManager {
    root: PathBuf,
    profiles: RefCell<HashMap<IdentityId, ProfileHandle>>,
}

impl std::fmt::Debug for ProfileManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileManager")
            .field("root", &self.root)
            .field("profile_count", &self.profiles.borrow().len())
            .finish()
    }
}

impl ProfileManager {
    /// Create a manager that roots every identity's storage under `root`. Each
    /// identity `i` gets `<root>/profile-<i>`.
    ///
    /// `root` MUST be exactly the engine's [`crate::EngineConfig::cache_path`], so
    /// that each profile directory is a *direct child* of CEF's `root_cache_path`.
    /// Any other layout makes CEF reject the profile (in-memory fallback or
    /// outright failure) — see the module docs. The caller is responsible for
    /// honouring this; `mote-cef` cannot read the engine config back from CEF to
    /// check it.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            profiles: RefCell::new(HashMap::new()),
        }
    }

    /// The profile-storage root all identities live under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the profile for `id`, creating it on first request. Subsequent calls
    /// for the same id return a clone of the same handle (same `RequestContext`).
    ///
    /// # Errors
    /// [`CefError::Profile`] if the context must be created and CEF fails to do
    /// so.
    pub fn get_or_create(&self, id: &IdentityId) -> Result<ProfileHandle> {
        if let Some(existing) = self.profiles.borrow().get(id) {
            return Ok(existing.clone());
        }
        let profile = ProfileHandle::create(id, &self.root)?;
        self.profiles
            .borrow_mut()
            .insert(id.clone(), profile.clone());
        Ok(profile)
    }

    /// Return the already-created profile for `id`, or `None` if it has not been
    /// created yet (does not create one).
    #[must_use]
    pub fn get(&self, id: &IdentityId) -> Option<ProfileHandle> {
        self.profiles.borrow().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_id_rejects_traversal_and_separators() {
        assert!(IdentityId::new("").is_err());
        assert!(IdentityId::new(".").is_err());
        assert!(IdentityId::new("..").is_err());
        assert!(IdentityId::new("a/b").is_err());
        assert!(IdentityId::new("a\\b").is_err());
        assert!(IdentityId::new("../escape").is_err());
        assert!(IdentityId::new("with\0null").is_err());
    }

    #[test]
    fn identity_id_accepts_safe_ids() {
        for ok in ["default", "work", "personal-2", "id_42", "alice.smith"] {
            assert_eq!(IdentityId::new(ok).unwrap().as_str(), ok);
        }
    }

    /// Mirrors the path scheme `ProfileHandle::create` uses (`<root>/profile-<id>`)
    /// so the layout contract is covered without a live CEF process.
    fn profile_dir(root: &Path, id: &IdentityId) -> PathBuf {
        root.join(format!("profile-{}", id.as_str()))
    }

    #[test]
    fn storage_path_is_root_joined_with_prefixed_identity() {
        let root = Path::new("/tmp/mote-profiles");
        let id = IdentityId::new("work").unwrap();
        assert_eq!(
            profile_dir(root, &id),
            Path::new("/tmp/mote-profiles/profile-work")
        );
    }

    #[test]
    fn distinct_identities_get_distinct_paths() {
        let root = Path::new("/tmp/mote-profiles");
        let a = IdentityId::new("alice").unwrap();
        let b = IdentityId::new("bob").unwrap();
        assert_ne!(profile_dir(root, &a), profile_dir(root, &b));
    }
}
