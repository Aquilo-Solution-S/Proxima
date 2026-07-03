//! Owner scoping primitive.
//!
//! Runtime mirror of the Lean `OwnerRef` boundary: rows carry a stable owner
//! reference; hosts resolve that reference into roles before authorization.

use uuid::Uuid;

use crate::{GroupId, UserId};

pub const WORLD_OWNER_UUID: Uuid = Uuid::from_u128(1);
pub const WORLD_GROUP_ID: GroupId = GroupId::new(WORLD_OWNER_UUID);

/// Row owner reference. This is stable persisted identity, not the resolved
/// role map used for authorization.
pub type Owner = OwnerRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OwnerRef {
    World,
    Personal(UserId),
    Group(GroupId),
}

impl OwnerRef {
    #[must_use]
    pub const fn world() -> Self {
        Self::World
    }

    #[must_use]
    pub const fn columns(self) -> (OwnerRefKind, Option<Uuid>) {
        match self {
            Self::World => (OwnerRefKind::World, None),
            Self::Personal(user) => (OwnerRefKind::Personal, Some(user.into_inner())),
            Self::Group(group) => (OwnerRefKind::Group, Some(group.into_inner())),
        }
    }

    /// Stable non-DB UUID component for deterministic hashes / object keys.
    ///
    /// Database owner columns use [`Self::columns`], where `World` is
    /// encoded as `owner_id = NULL`. This helper is intentionally not a DB
    /// column adapter.
    #[must_use]
    pub const fn stable_key_uuid(self) -> Uuid {
        match self {
            Self::World => WORLD_OWNER_UUID,
            Self::Personal(user) => user.into_inner(),
            Self::Group(group) => group.into_inner(),
        }
    }

    /// Canonical runtime/API owner key.
    ///
    /// This is an external protocol convention, not a storage adapter:
    /// `world:<uuid>`, `personal:<uuid>`, or `group:<uuid>`.
    #[must_use]
    pub fn external_key(self) -> String {
        let (kind, _) = self.columns();
        format!("{}:{}", kind.as_str(), self.stable_key_uuid())
    }
}

/// Parse a canonical runtime/API owner key.
///
/// # Errors
///
/// Returns [`OwnerExternalKeyParseError`] when the key is not exactly
/// `world:<uuid>`, `personal:<uuid>`, or `group:<uuid>`, or when the
/// `world` UUID is not Proxima's singleton world owner UUID.
pub fn parse_external_key(raw: &str) -> Result<OwnerRef, OwnerExternalKeyParseError> {
    let (kind, raw_id) = raw
        .split_once(':')
        .ok_or(OwnerExternalKeyParseError::InvalidFormat)?;
    let id = Uuid::parse_str(raw_id).map_err(|source| OwnerExternalKeyParseError::InvalidUuid {
        kind: kind.to_string(),
        source,
    })?;
    match kind {
        "world" if id == WORLD_OWNER_UUID => Ok(OwnerRef::World),
        "world" => Err(OwnerExternalKeyParseError::InvalidWorldUuid { id }),
        "personal" => Ok(OwnerRef::Personal(UserId::new(id))),
        "group" => Ok(OwnerRef::Group(GroupId::new(id))),
        _ => Err(OwnerExternalKeyParseError::InvalidKind {
            kind: kind.to_string(),
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerExternalKeyParseError {
    #[error("owner external key must be `world:<uuid>`, `personal:<uuid>`, or `group:<uuid>`")]
    InvalidFormat,
    #[error("owner external key kind {kind:?} is not supported")]
    InvalidKind { kind: String },
    #[error("owner external key {kind:?} id is not a UUID: {source}")]
    InvalidUuid { kind: String, source: uuid::Error },
    #[error(
        "world owner external key must use the singleton UUID 00000000-0000-0000-0000-000000000001, got {id}"
    )]
    InvalidWorldUuid { id: Uuid },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.owner_ref_kind", rename_all = "lowercase")]
pub enum OwnerRefKind {
    World,
    Personal,
    Group,
}

impl OwnerRefKind {
    #[must_use]
    pub const fn of(owner: &OwnerRef) -> Self {
        match owner {
            OwnerRef::World => Self::World,
            OwnerRef::Personal(_) => Self::Personal,
            OwnerRef::Group(_) => Self::Group,
        }
    }

    #[must_use]
    pub const fn with_uuid(self, id: Option<Uuid>) -> Option<OwnerRef> {
        match (self, id) {
            (Self::World, None) => Some(OwnerRef::World),
            (Self::Personal, Some(id)) => Some(OwnerRef::Personal(crate::UserId::new(id))),
            (Self::Group, Some(id)) => Some(OwnerRef::Group(crate::GroupId::new(id))),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Personal => "personal",
            Self::Group => "group",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_refs_are_stable_handles_not_resolved_roles() {
        let user = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());

        assert_eq!(OwnerRefKind::of(&OwnerRef::World), OwnerRefKind::World);
        assert_eq!(
            OwnerRefKind::of(&OwnerRef::Personal(user)),
            OwnerRefKind::Personal
        );
        assert_eq!(
            OwnerRefKind::of(&OwnerRef::Group(group)),
            OwnerRefKind::Group
        );
    }

    #[test]
    fn owner_external_keys_round_trip() {
        let personal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));

        assert_eq!(
            parse_external_key(&OwnerRef::World.external_key()).expect("world key"),
            OwnerRef::World
        );
        assert_eq!(
            parse_external_key(&personal.external_key()).expect("personal key"),
            personal
        );
        assert_eq!(
            parse_external_key(&group.external_key()).expect("group key"),
            group
        );
    }

    #[test]
    fn owner_external_key_rejects_non_canonical_forms() {
        assert!(matches!(
            parse_external_key("world"),
            Err(OwnerExternalKeyParseError::InvalidFormat)
        ));
        assert!(matches!(
            parse_external_key("personal:not-a-uuid"),
            Err(OwnerExternalKeyParseError::InvalidUuid { .. })
        ));
        assert!(matches!(
            parse_external_key("org:00000000-0000-0000-0000-000000000000"),
            Err(OwnerExternalKeyParseError::InvalidKind { .. })
        ));
        assert!(matches!(
            parse_external_key("world:00000000-0000-0000-0000-000000000000"),
            Err(OwnerExternalKeyParseError::InvalidWorldUuid { .. })
        ));
    }
}
