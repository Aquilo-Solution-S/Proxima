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
    pub const fn stable_kind(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Personal(_) => "personal",
            Self::Group(_) => "group",
        }
    }

    /// Transitional SQL columns for the pre-PR2 schema. `World` is represented
    /// by the reserved group UUID until the v0.0.4 baseline removes the old
    /// split owner columns.
    #[must_use]
    pub const fn columns(self) -> (OwnerRefKind, Uuid) {
        match self {
            Self::World => (OwnerRefKind::Group, WORLD_OWNER_UUID),
            Self::Personal(user) => (OwnerRefKind::User, user.into_inner()),
            Self::Group(group) => (OwnerRefKind::Group, group.into_inner()),
        }
    }
}

/// Transitional SQL discriminant for the pre-PR2 schema. Not an authorization
/// primitive; use `OwnerRef` plus server-resolved roles at API boundaries.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.owner_principal_kind")]
pub enum OwnerRefKind {
    User,
    Group,
}

impl OwnerRefKind {
    #[must_use]
    pub const fn of(owner: &OwnerRef) -> Self {
        match owner {
            OwnerRef::World | OwnerRef::Group(_) => Self::Group,
            OwnerRef::Personal(_) => Self::User,
        }
    }

    #[must_use]
    pub const fn with_uuid(self, id: Uuid) -> OwnerRef {
        match (self, id.as_u128()) {
            (Self::Group, 1) => OwnerRef::World,
            (Self::User, _) => OwnerRef::Personal(crate::UserId::new(id)),
            (Self::Group, _) => OwnerRef::Group(crate::GroupId::new(id)),
        }
    }

    /// Stable bytes for non-SQL contexts. Matches the legacy SQL enum label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Group => "Group",
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

        assert_eq!(OwnerRef::World.stable_kind(), "world");
        assert_eq!(OwnerRef::Personal(user).stable_kind(), "personal");
        assert_eq!(OwnerRef::Group(group).stable_kind(), "group");
    }
}
