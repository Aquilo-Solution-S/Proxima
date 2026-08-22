//! Owner scoping primitive.
//!
//! Runtime mirror of the Lean `OwnerRef` boundary: rows carry a stable owner
//! reference; hosts resolve that reference into roles before authorization.

use uuid::Uuid;

use crate::{GroupId, UserId};

/// Row owner reference. This is stable persisted identity, not the resolved
/// role map used for authorization.
pub type Owner = OwnerRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OwnerRef {
    Personal(UserId),
    Group(GroupId),
}

impl OwnerRef {
    /// Storage columns for this reference: the kind discriminant and the
    /// UUID. Every owner has a UUID — there is no id-less owner kind.
    #[must_use]
    pub const fn columns(self) -> (OwnerRefKind, Uuid) {
        match self {
            Self::Personal(user) => (OwnerRefKind::Personal, user.into_inner()),
            Self::Group(group) => (OwnerRefKind::Group, group.into_inner()),
        }
    }

    /// Stored `owner_id`, never NULL.
    #[must_use]
    pub const fn stored_owner_id(self) -> Uuid {
        self.stable_key_uuid()
    }

    /// Stable non-DB UUID component for deterministic hashes / object keys.
    ///
    /// This value is what `owner_id` stores (NOT NULL).
    #[must_use]
    pub const fn stable_key_uuid(self) -> Uuid {
        match self {
            Self::Personal(user) => user.into_inner(),
            Self::Group(group) => group.into_inner(),
        }
    }

    /// Canonical runtime/API owner key.
    ///
    /// This is an external protocol convention, not a storage adapter:
    /// `personal:<uuid>` or `group:<uuid>`.
    #[must_use]
    pub fn external_key(self) -> String {
        let (kind, id) = self.columns();
        format!("{}:{}", kind.as_str(), id)
    }
}

/// Parse a canonical runtime/API owner key.
///
/// # Errors
///
/// Returns [`OwnerExternalKeyParseError`] when the key is not exactly
/// `personal:<uuid>` or `group:<uuid>`.
pub fn parse_external_key(raw: &str) -> Result<OwnerRef, OwnerExternalKeyParseError> {
    let (kind, raw_id) = raw
        .split_once(':')
        .ok_or(OwnerExternalKeyParseError::InvalidFormat)?;
    let id = Uuid::parse_str(raw_id).map_err(|source| OwnerExternalKeyParseError::InvalidUuid {
        kind: kind.to_string(),
        source,
    })?;
    match kind {
        "personal" => Ok(OwnerRef::Personal(UserId::new(id))),
        "group" => Ok(OwnerRef::Group(GroupId::new(id))),
        _ => Err(OwnerExternalKeyParseError::InvalidKind {
            kind: kind.to_string(),
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerExternalKeyParseError {
    #[error("owner external key must be `personal:<uuid>` or `group:<uuid>`")]
    InvalidFormat,
    #[error("owner external key kind {kind:?} is not supported")]
    InvalidKind { kind: String },
    #[error("owner external key {kind:?} id is not a UUID: {source}")]
    InvalidUuid { kind: String, source: uuid::Error },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.owner_kind", rename_all = "lowercase")]
pub enum OwnerRefKind {
    Personal,
    Group,
}

impl OwnerRefKind {
    #[must_use]
    pub const fn of(owner: &OwnerRef) -> Self {
        match owner {
            OwnerRef::Personal(_) => Self::Personal,
            OwnerRef::Group(_) => Self::Group,
        }
    }

    /// Rebuild the owner reference from its stored columns. Total: every
    /// owner kind carries a UUID.
    #[must_use]
    pub const fn with_uuid(self, id: Uuid) -> OwnerRef {
        match self {
            Self::Personal => OwnerRef::Personal(crate::UserId::new(id)),
            Self::Group => OwnerRef::Group(crate::GroupId::new(id)),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
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
            parse_external_key("personal"),
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
            parse_external_key("world:00000000-0000-0000-0000-000000000001"),
            Err(OwnerExternalKeyParseError::InvalidKind { .. })
        ));
    }
}
