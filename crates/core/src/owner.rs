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
}
