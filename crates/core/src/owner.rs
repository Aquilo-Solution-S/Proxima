//! Owner scoping primitive.
//!
//! See docs/01-event-source.md §"Owner — scoping primitive" for semantics.

use crate::{GroupId, OrgId, UserId};

/// Storage/row annotation assembled from an [`AuthzContext`](crate::AuthzContext).
///
/// Public request surfaces carry [`Principal`], not `Owner`. The verb layer
/// checks that principal against the caller identity, then stamps `org_id`
/// from auth context to reconstruct this pair for rows, storage drafts, wake
/// internals, and stable hash inputs. `org_id` is NOT part of the access
/// predicate (AGENTS.md invariant 4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct Owner {
    pub principal: Principal,
    pub org_id: OrgId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum Principal {
    User(UserId),
    Group(GroupId),
}

/// Discriminant tag for `Principal`, mirrors the SQL enum
/// `proxima_core.owner_principal_kind`. Storage rows split a
/// `Principal` across two columns (`owner_principal_kind` +
/// `owner_principal_id`); `FromRow` decoders read this tag.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    sqlx::Type,
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
