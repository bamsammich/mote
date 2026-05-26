//! Shared `serde` helpers for `mote-types` ID newtypes.
//!
//! `mote-types` does not depend on `serde`, so `IdentityId`, `WorkspaceId`,
//! and `TabId` have no built-in `Serialize`/`Deserialize` impls. Each helper
//! module in this file marshals the newtype as its underlying `u64`.
//!
//! Usage: `#[serde(with = "crate::serde_helpers::identity_id")]`.
//!
//! # Clippy notes
//!
//! The `with = "module"` serde protocol requires `fn serialize(val: &FieldType, s: S)`.
//! For Copy field types, this triggers `clippy::trivially_copy_pass_by_ref`; for
//! `Option<T>` fields, it triggers `clippy::ref_option`. Both are structural
//! false positives: the signatures are mandated by serde's API, not our choice.

/// Serde adapter for [`mote_types::IdentityId`].
pub(crate) mod identity_id {
    use mote_types::IdentityId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes an [`IdentityId`] as a `u64`.
    // `&IdentityId` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(id: &IdentityId, s: S) -> Result<S::Ok, S::Error> {
        id.get().serialize(s)
    }

    /// Deserializes a `u64` as an [`IdentityId`].
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<IdentityId, D::Error> {
        u64::deserialize(d).map(IdentityId::new)
    }
}

/// Serde adapter for optional [`mote_types::IdentityId`] fields.
pub(crate) mod opt_identity_id {
    use mote_types::IdentityId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes an `Option<IdentityId>` as an optional `u64`.
    // `&Option<T>` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::ref_option)]
    pub(crate) fn serialize<S: Serializer>(
        id: &Option<IdentityId>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        id.map(IdentityId::get).serialize(s)
    }

    /// Deserializes an optional `u64` as `Option<IdentityId>`.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<IdentityId>, D::Error> {
        Option::<u64>::deserialize(d).map(|o| o.map(IdentityId::new))
    }
}

/// Serde adapter for [`mote_types::WorkspaceId`].
pub(crate) mod workspace_id {
    use mote_types::WorkspaceId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes a [`WorkspaceId`] as a `u64`.
    // `&WorkspaceId` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(id: &WorkspaceId, s: S) -> Result<S::Ok, S::Error> {
        id.get().serialize(s)
    }

    /// Deserializes a `u64` as a [`WorkspaceId`].
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<WorkspaceId, D::Error> {
        u64::deserialize(d).map(WorkspaceId::new)
    }
}

/// Serde adapter for optional [`mote_types::TabId`] fields.
pub(crate) mod opt_tab_id {
    use mote_types::TabId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes an `Option<TabId>` as an optional `u64`.
    // `&Option<T>` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::ref_option)]
    pub(crate) fn serialize<S: Serializer>(id: &Option<TabId>, s: S) -> Result<S::Ok, S::Error> {
        id.map(TabId::get).serialize(s)
    }

    /// Deserializes an optional `u64` as `Option<TabId>`.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<TabId>, D::Error> {
        Option::<u64>::deserialize(d).map(|o| o.map(TabId::new))
    }
}

/// Serde adapter for [`mote_types::TabId`].
pub(crate) mod tab_id {
    use mote_types::TabId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes a [`TabId`] as a `u64`.
    // `&TabId` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(id: &TabId, s: S) -> Result<S::Ok, S::Error> {
        id.get().serialize(s)
    }

    /// Deserializes a `u64` as a [`TabId`].
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TabId, D::Error> {
        u64::deserialize(d).map(TabId::new)
    }
}

/// Serde adapter for slices of [`mote_types::TabId`].
pub(crate) mod tab_id_vec {
    use mote_types::TabId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes a `Vec<TabId>` as a `Vec<u64>`.
    pub(crate) fn serialize<S: Serializer>(ids: &[TabId], s: S) -> Result<S::Ok, S::Error> {
        let raw: Vec<u64> = ids.iter().map(|t| t.get()).collect();
        raw.serialize(s)
    }

    /// Deserializes a `Vec<u64>` as a `Vec<TabId>`.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TabId>, D::Error> {
        Vec::<u64>::deserialize(d).map(|v| v.into_iter().map(TabId::new).collect())
    }
}

/// Serde adapter for [`std::time::SystemTime`] as Unix seconds.
pub(crate) mod system_time {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes a [`SystemTime`] as seconds since Unix epoch.
    pub(crate) fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        t.duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            .serialize(s)
    }

    /// Deserializes seconds since Unix epoch as a [`SystemTime`].
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        u64::deserialize(d).map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
    }
}

/// Serde adapter for optional [`std::time::SystemTime`] as optional Unix seconds.
pub(crate) mod opt_system_time {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes an `Option<SystemTime>` as an optional `u64` (Unix seconds).
    // `&Option<T>` is required by serde's `with = "..."` calling convention.
    #[allow(clippy::ref_option)]
    pub(crate) fn serialize<S: Serializer>(
        t: &Option<SystemTime>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        t.map(|ts| {
            ts.duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs()
        })
        .serialize(s)
    }

    /// Deserializes an optional `u64` (Unix seconds) as `Option<SystemTime>`.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<u64>::deserialize(d).map(|o| o.map(|secs| UNIX_EPOCH + Duration::from_secs(secs)))
    }
}
