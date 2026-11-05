//! Authenticated, immutable owner scope.
//!
//! `OwnerScope` is deliberately opaque. Production instances are sealed from
//! verified roles by [`crate::authenticate`] or the boot-authority-gated
//! [`crate::AuthzContext::for_system`] and
//! [`crate::AuthzContext::for_insecure_single_owner`] paths. Callers can only
//! inspect or narrow the resulting witness.

use std::time::SystemTime;

use crate::{AccessKind, GroupId, OwnerId, OwnerRoles, Role, UserId, WriterAttribution};

/// Server-authenticated owner authority carried alongside an [`AuthzContext`](crate::AuthzContext).
///
/// The fields are private and there is no public constructor.  In particular,
/// owner headers and ordinary `AuthzContext` compatibility constructors cannot
/// produce this witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerScope {
    pub(crate) roles: OwnerRoles,
    writer: WriterAttribution,
    expires_at: Option<SystemTime>,
    selected: Option<OwnerId>,
    goal_principal: Option<crate::goal_collaboration::GoalPrincipalScope>,
}

impl OwnerScope {
    pub(crate) fn from_verified_roles(
        roles: OwnerRoles,
        expires_at: Option<SystemTime>,
        writer: WriterAttribution,
    ) -> Self {
        Self {
            roles,
            writer,
            expires_at,
            selected: None,
            goal_principal: None,
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
            writer: self.writer,
            expires_at: self.expires_at,
            selected: None,
            goal_principal: self.goal_principal,
        })
    }

    pub(crate) fn narrow(&self, owner: OwnerId) -> Option<Self> {
        let subject = self.roles.subject();
        let role = self.roles.role_for(&owner)?;
        Some(Self {
            roles: OwnerRoles::scoped_to(subject, owner, role),
            writer: self.writer,
            expires_at: self.expires_at,
            selected: Some(owner),
            goal_principal: None,
        })
    }

    /// Add a verified Goal principal. Only the authentication engine may mint
    /// this lane; it is never derived from owner roles or a forwarded scope.
    pub(crate) fn with_goal_principal(
        mut self,
        principal: crate::goal_collaboration::GoalPrincipalScope,
    ) -> Self {
        if self.selected.is_none() {
            self.goal_principal = Some(principal);
        }
        self
    }

    #[must_use]
    pub fn goal_principal(&self) -> Option<&crate::goal_collaboration::GoalPrincipalScope> {
        if self.is_expired() {
            None
        } else {
            self.goal_principal.as_ref()
        }
    }

    pub(crate) const fn confined_to(&self) -> Option<OwnerId> {
        self.selected
    }

    /// The immutable writer captured when authentication sealed this scope.
    ///
    /// ```
    /// # use proxiima_core::{OwnerScope, WriterAttribution};
    /// fn inspect(scope: &OwnerScope) -> WriterAttribution {
    ///     scope.writer()
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// # use proxiima_core::{OwnerScope, WriterAttribution};
    /// # fn replace(scope: &mut OwnerScope, writer: WriterAttribution) {
    /// scope.writer = writer;
    /// # }
    /// ```
    #[must_use]
    pub const fn writer(&self) -> WriterAttribution {
        self.writer
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
    pub fn role_for_owner(&self, owner: &OwnerId) -> Option<Role> {
        self.roles.role_for(owner)
    }

    #[must_use]
    pub fn may_read(&self, owner: &OwnerId, kind: AccessKind) -> bool {
        !self.is_expired() && self.roles.may_read(owner, kind)
    }

    #[must_use]
    pub fn may_write(&self, owner: &OwnerId, kind: AccessKind) -> bool {
        !self.is_expired() && self.roles.may_write(owner, kind)
    }

    #[must_use]
    pub fn readable_owners(&self, kind: AccessKind) -> Vec<OwnerId> {
        if self.is_expired() {
            Vec::new()
        } else {
            self.roles.readable_owners(kind)
        }
    }

    #[must_use]
    pub fn managed_owners(&self) -> Vec<OwnerId> {
        self.readable_owners(AccessKind::Fact)
            .into_iter()
            .filter(|owner| self.role_for_owner(owner).is_some_and(Role::manages))
            .collect()
    }

    #[must_use]
    pub fn writable_owners(&self, kind: AccessKind) -> Vec<OwnerId> {
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
            OwnerRoles::for_subject(subject, [(OwnerId::from(group), Role::editor())]).unwrap();
        let writer = WriterAttribution {
            by: subject.into_inner(),
            via: Some(uuid::Uuid::now_v7()),
        };
        let original = OwnerScope::from_verified_roles(roles, None, writer);
        let narrowed = original.narrow(OwnerId::from(group)).unwrap();
        assert!(narrowed.narrow(OwnerId::from(subject)).is_none());
        assert!(narrowed.add_group_role(other, Role::admin()).is_none());
        assert!(narrowed.narrow(OwnerId::from(group)).is_some());
        assert!(original.narrow(OwnerId::from(subject)).is_some());
        assert_eq!(narrowed.writer(), writer);
        assert_eq!(
            original
                .add_group_role(other, Role::viewer())
                .unwrap()
                .writer(),
            writer
        );
    }
}
