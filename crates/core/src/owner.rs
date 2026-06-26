//! Owner scoping primitive.
//!
//! See docs/01-event-source.md §"Owner — scoping primitive" for semantics.

use crate::{GroupId, UserId};

/// Owner scoping annotation — now a pure alias of [`Principal`].
///
/// Track B (S0, 2026-06): the former tenant scalar was removed from Core
/// entirely. There is no tenant field in the access predicate, the identity
/// hashes, or storage rows; tenancy is a flavor/app concern. Mirrors the Lean
/// kernel `def Owner : Type := Principal` (docs/lean/Causa/Owner.lean).
pub type Owner = Principal;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Principal {
    User(UserId),
    Group(GroupId),
}

impl Principal {
    #[must_use]
    pub fn columns(&self) -> (OwnerPrincipalKind, uuid::Uuid) {
        match self {
            Self::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
            Self::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
        }
    }
}

/// Discriminant tag for `Principal`, mirrors the SQL enum
/// `proxima_core.owner_principal_kind`. Storage rows split a
/// `Principal` across two columns (`owner_principal_kind` +
/// `owner_principal_id`); `FromRow` decoders read this tag.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.owner_principal_kind")]
pub enum OwnerPrincipalKind {
    User,
    Group,
}

impl OwnerPrincipalKind {
    #[must_use]
    pub fn of(principal: &Principal) -> Self {
        match principal {
            Principal::User(_) => Self::User,
            Principal::Group(_) => Self::Group,
        }
    }

    #[must_use]
    pub fn with_uuid(self, id: uuid::Uuid) -> Principal {
        match self {
            Self::User => Principal::User(crate::UserId::new(id)),
            Self::Group => Principal::Group(crate::GroupId::new(id)),
        }
    }

    /// Stable bytes for non-SQL contexts (e.g. external-key hashing).
    /// Matches the SQL enum label exactly.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Group => "Group",
        }
    }
}
