//! Authenticated, immutable owner scope.
//!
//! `OwnerScope` is deliberately opaque.  Production instances are minted by
//! [`crate::authenticate`] after the host authenticator has accepted the
//! credentials; callers can only inspect or narrow the resulting witness.

use std::time::SystemTime;

use crate::{AccessKind, GroupId, OwnerRef, OwnerRoles, Role, UserId};

/// Server-authenticated owner authority carried alongside an [`AuthzContext`](crate::AuthzContext).
///
/// The fields are private and there is no public constructor.  In particular,
/// owner headers and ordinary `AuthzContext` compatibility constructors cannot
/// produce this witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerScope {
    pub(crate) roles: OwnerRoles,
    expires_at: Option<SystemTime>,
    selected: Option<OwnerRef>,
}

impl OwnerScope {
    pub(crate) fn from_verified_roles(roles: OwnerRoles, expires_at: Option<SystemTime>) -> Self {
        Self {
            roles,
            expires_at,
            selected: None,
        }
    }

    pub(crate) fn add_group_role(&self, group: GroupId, role: Role) -> Option<Self> {
        // On-demand host resolution belongs before request narrowing. A
        // narrowed witness can never grow its authority again.
        if self.selected.is_some() || self.is_expired() {
            return None;
        }
        Some(Self {
            roles: self.roles.clone().with_group_role(group, role),
            expires_at: self.expires_at,
            selected: None,
        })
    }

    pub(crate) fn narrow(&self, owner: OwnerRef) -> Option<Self> {
        let subject = self.roles.subject();
        let role = self.roles.role_for(&owner)?;
        Some(Self {
            roles: OwnerRoles::scoped_to(subject, owner, role),
            expires_at: self.expires_at,
            selected: Some(owner),
        })
    }

    #[must_use]
    pub const fn subject(&self) -> UserId {
        self.roles.subject()
    }

    #[must_use]
    pub fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|deadline| deadline <= SystemTime::now())
    }

    #[must_use]
    pub fn role_for_owner(&self, owner: &OwnerRef) -> Option<Role> {
        self.roles.role_for(owner)
    }

    #[must_use]
    pub fn may_read(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        !self.is_expired() && self.roles.may_read(owner, kind)
    }

    #[must_use]
    pub fn may_write(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        !self.is_expired() && self.roles.may_write(owner, kind)
    }

    #[must_use]
    pub fn readable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        if self.is_expired() {
            Vec::new()
        } else {
            self.roles.readable_owners(kind)
        }
    }

    #[must_use]
    pub fn managed_owners(&self) -> Vec<OwnerRef> {
        self.readable_owners(AccessKind::Fact)
            .into_iter()
            .filter(|owner| self.role_for_owner(owner).is_some_and(Role::manages))
            .collect()
    }

    #[must_use]
    pub fn writable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        if self.is_expired() {
            Vec::new()
        } else {
            self.roles.writable_owners(kind)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrowing_cannot_restore_personal_or_add_another_group() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let other = GroupId::new(uuid::Uuid::now_v7());
        let roles =
            OwnerRoles::for_subject(subject, [(OwnerRef::Group(group), Role::editor())]).unwrap();
        let original = OwnerScope::from_verified_roles(roles, None);
        let narrowed = original.narrow(OwnerRef::Group(group)).unwrap();
        assert!(narrowed.narrow(OwnerRef::Personal(subject)).is_none());
        assert!(narrowed.add_group_role(other, Role::admin()).is_none());
        assert!(narrowed.narrow(OwnerRef::Group(group)).is_some());
        assert!(original.narrow(OwnerRef::Personal(subject)).is_some());
    }
}
