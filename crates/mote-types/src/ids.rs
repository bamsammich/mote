//! Strongly-typed id newtypes for identities, workspaces, and tabs.
//!
//! Each id wraps a [`u64`] but is a distinct type, so the compiler rejects
//! passing (say) a [`TabId`] where a [`WorkspaceId`] is expected. The backing
//! integer is an opaque handle; its allocation strategy (`SQLite` rowid, counter,
//! …) is an owning-crate concern, not a `mote-types` concern.

use std::fmt;

/// Declares a transparent `u64` id newtype with the shared accessor surface.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[doc = concat!("An opaque identifier for a ", $what, ".")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            #[doc = concat!("Wraps a raw `u64` as a [`", stringify!($name), "`].")]
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// Returns the underlying raw `u64`.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_newtype!(IdentityId, "user identity (Chromium profile)");
id_newtype!(WorkspaceId, "workspace");
id_newtype!(TabId, "tab");
