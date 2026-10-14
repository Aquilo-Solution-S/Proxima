//! Owner scoping primitive.
//!
//! Runtime mirror of the Lean `OwnerId` boundary (`docs/lean/Causa/Identity.lean`):
//! a row carries one stable owner id; the host resolves that id into roles
//! before authorization. A resolved owner is always a group, and a user is the
//! personal group whose only member is that user, so a user's id is its own
//! owner id. Whether an id names a group or a user is Proxima's internal
//! registry (`proxima_core.owners.kind`), never part of this type.

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::{GroupId, UserId};

/// Stable persisted owner reference: one owner id, a UUID. This is not the
/// resolved role map used for authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct OwnerId(Uuid);

impl OwnerId {
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// The stored `owner_id`, and the UUID every owner-keyed hash or object
    /// key carries.
    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }

    /// The same id read as a member's id: the owner a user holds the personal
    /// role in.
    #[must_use]
    pub const fn as_user(self) -> UserId {
        UserId::new(self.0)
    }

    /// The same id read as a group's id.
    #[must_use]
    pub const fn as_group(self) -> GroupId {
        GroupId::new(self.0)
    }
}

impl From<UserId> for OwnerId {
    fn from(user: UserId) -> Self {
        Self(user.into_inner())
    }
}

impl From<GroupId> for OwnerId {
    fn from(group: GroupId) -> Self {
        Self(group.into_inner())
    }
}

impl From<Uuid> for OwnerId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<OwnerId> for Uuid {
    fn from(owner: OwnerId) -> Self {
        owner.0
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), formatter)
    }
}

/// An owner id on the wire is exactly a hyphenated UUID.
#[derive(Debug, thiserror::Error)]
#[error("owner id must be a hyphenated UUID")]
pub struct OwnerIdParseError;

impl FromStr for OwnerId {
    type Err = OwnerIdParseError;

    /// Parse the wire form: a 36-character hyphenated UUID and nothing else.
    /// A prefixed (`kind:<uuid>`), braced, URN or simple form is refused like
    /// any malformed id.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.len() != 36 {
            return Err(OwnerIdParseError);
        }
        Uuid::try_parse(raw)
            .map(Self)
            .map_err(|_| OwnerIdParseError)
    }
}

impl serde::Serialize for OwnerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for OwnerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_id_is_the_uuid_of_its_user_or_group() {
        let id = Uuid::now_v7();
        assert_eq!(
            OwnerId::from(UserId::new(id)),
            OwnerId::from(GroupId::new(id))
        );
        assert_eq!(OwnerId::from(UserId::new(id)).into_inner(), id);
    }

    #[test]
    fn an_owner_id_round_trips_its_wire_form() {
        let owner = OwnerId::new(Uuid::now_v7());
        assert_eq!(owner.to_string().parse::<OwnerId>().expect("wire"), owner);
        assert_eq!(
            serde_json::to_value(owner).expect("serialize"),
            serde_json::Value::String(owner.to_string())
        );
    }

    #[test]
    fn an_old_kind_prefixed_owner_key_is_refused() {
        let id = Uuid::now_v7();
        for raw in [
            format!("personal:{id}"),
            format!("group:{id}"),
            format!("{{{id}}}"),
            format!("urn:uuid:{id}"),
            id.simple().to_string(),
            "personal".to_owned(),
            String::new(),
        ] {
            assert!(raw.parse::<OwnerId>().is_err(), "{raw} must be refused");
            assert!(
                serde_json::from_value::<OwnerId>(serde_json::Value::String(raw.clone())).is_err(),
                "{raw} must not deserialize"
            );
        }
    }
}
