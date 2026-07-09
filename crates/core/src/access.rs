//! Owner-role access vocabulary.

use std::collections::HashMap;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::{GoalId, GroupId, MemoryId, OwnerRef, UserId};

#[must_use]
pub const fn world() -> OwnerRef {
    OwnerRef::World
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AccessKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl AccessKind {
    pub const ALL: [Self; 4] = [Self::Fact, Self::Abstraction, Self::Perspective, Self::Goal];

    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Fact => 1,
            Self::Abstraction => 2,
            Self::Perspective => 3,
            Self::Goal => 4,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum AccessCeiling {
    None,
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl AccessCeiling {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Fact => 1,
            Self::Abstraction => 2,
            Self::Perspective => 3,
            Self::Goal => 4,
        }
    }

    #[must_use]
    pub const fn allows(self, kind: AccessKind) -> bool {
        kind.rank() <= self.rank()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AccessError {
    #[error("write ceiling exceeds read ceiling")]
    WriteExceedsRead,
    #[error("world and personal roles are derived, not resolver-provided")]
    DerivedOwnerOverride,
    #[error("owner access resolution failed: {0}")]
    Resolution(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Role {
    read: AccessCeiling,
    write: AccessCeiling,
    manage: bool,
}

impl Role {
    /// # Errors
    ///
    /// Returns [`AccessError::WriteExceedsRead`] when `write` is above `read`.
    pub const fn new(
        read: AccessCeiling,
        write: AccessCeiling,
        manage: bool,
    ) -> Result<Self, AccessError> {
        if write.rank() > read.rank() {
            return Err(AccessError::WriteExceedsRead);
        }
        Ok(Self {
            read,
            write,
            manage,
        })
    }

    #[must_use]
    pub const fn personal() -> Self {
        Self {
            read: AccessCeiling::Goal,
            write: AccessCeiling::Goal,
            manage: false,
        }
    }

    #[must_use]
    pub const fn viewer() -> Self {
        Self {
            read: AccessCeiling::Goal,
            write: AccessCeiling::None,
            manage: false,
        }
    }

    #[must_use]
    pub const fn ingest() -> Self {
        Self {
            read: AccessCeiling::Fact,
            write: AccessCeiling::Fact,
            manage: false,
        }
    }

    #[must_use]
    pub const fn editor() -> Self {
        Self {
            read: AccessCeiling::Goal,
            write: AccessCeiling::Perspective,
            manage: false,
        }
    }

    #[must_use]
    pub const fn admin() -> Self {
        Self {
            read: AccessCeiling::Goal,
            write: AccessCeiling::Goal,
            manage: true,
        }
    }

    #[must_use]
    pub const fn may_read(self, kind: AccessKind) -> bool {
        self.read.allows(kind)
    }

    #[must_use]
    pub const fn may_write(self, kind: AccessKind) -> bool {
        self.write.allows(kind)
    }

    #[must_use]
    pub const fn manages(self) -> bool {
        self.manage
    }

    #[must_use]
    pub const fn read_ceiling(self) -> AccessCeiling {
        self.read
    }

    #[must_use]
    pub const fn write_ceiling(self) -> AccessCeiling {
        self.write
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRoles {
    subject: UserId,
    roles: HashMap<OwnerRef, Role>,
}

impl OwnerRoles {
    /// # Errors
    ///
    /// Returns [`AccessError::DerivedOwnerOverride`] if the resolver tries to
    /// provide World or Personal roles; those are derived by the kernel rules.
    pub fn for_subject<I>(subject: UserId, group_roles: I) -> Result<Self, AccessError>
    where
        I: IntoIterator<Item = (OwnerRef, Role)>,
    {
        let mut roles = HashMap::new();
        roles.insert(OwnerRef::World, Role::viewer());
        roles.insert(OwnerRef::Personal(subject), Role::personal());
        for (owner, role) in group_roles {
            match owner {
                OwnerRef::Group(_) => {
                    roles.insert(owner, role);
                }
                OwnerRef::World | OwnerRef::Personal(_) => {
                    return Err(AccessError::DerivedOwnerOverride);
                }
            }
        }
        Ok(Self { subject, roles })
    }

    #[must_use]
    pub const fn subject(&self) -> UserId {
        self.subject
    }

    #[must_use]
    pub(crate) fn scoped_to(subject: UserId, owner: OwnerRef, role: Role) -> Self {
        let mut roles = HashMap::new();
        roles.insert(OwnerRef::World, Role::viewer());
        roles.insert(owner, role);
        Self { subject, roles }
    }

    #[must_use]
    pub fn empty_for_subject(subject: UserId) -> Self {
        let mut roles = HashMap::new();
        roles.insert(OwnerRef::World, Role::viewer());
        roles.insert(OwnerRef::Personal(subject), Role::personal());
        Self { subject, roles }
    }

    #[must_use]
    pub fn role_for(&self, owner: &OwnerRef) -> Option<Role> {
        match *owner {
            OwnerRef::Personal(user) if user != self.subject => None,
            _ => self.roles.get(owner).copied(),
        }
    }

    #[must_use]
    pub fn may_read(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        self.role_for(owner).is_some_and(|role| role.may_read(kind))
    }

    #[must_use]
    pub fn may_write(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        self.role_for(owner)
            .is_some_and(|role| role.may_write(kind))
    }

    #[must_use]
    pub fn may_manage(&self, owner: &OwnerRef) -> bool {
        match owner {
            OwnerRef::World | OwnerRef::Personal(_) => false,
            OwnerRef::Group(_) => self.role_for(owner).is_some_and(Role::manages),
        }
    }

    #[must_use]
    pub fn readable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        self.roles
            .iter()
            .filter_map(|(owner, role)| role.may_read(kind).then_some(*owner))
            .collect()
    }

    #[must_use]
    pub fn writable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        self.roles
            .iter()
            .filter_map(|(owner, role)| role.may_write(kind).then_some(*owner))
            .collect()
    }
}

#[async_trait]
pub trait OwnerAccessPort: Send + Sync {
    /// # Errors
    ///
    /// Returns [`AccessError`] when the host cannot resolve access roles.
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.membership_relation",
    rename_all = "lowercase"
)]
pub enum Relation {
    Admin,
    Editor,
    Viewer,
    Ingest,
}

impl Relation {
    #[must_use]
    pub const fn dominates(self, required: Self) -> bool {
        use Relation::{Admin, Editor, Ingest, Viewer};
        matches!(
            (self, required),
            (Editor, Editor | Viewer) | (Viewer, Viewer) | (Admin, Admin) | (Ingest, Ingest)
        )
    }

    #[must_use]
    pub const fn denied_message(self) -> &'static str {
        match self {
            Self::Admin => "requires admin on this owner",
            Self::Editor => "requires editor on this owner",
            Self::Viewer => "requires viewer on this owner",
            Self::Ingest => "requires ingest on this owner",
        }
    }

    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Admin => Role::admin(),
            Self::Editor => Role::editor(),
            Self::Viewer => Role::viewer(),
            Self::Ingest => Role::ingest(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityId {
    Memory(MemoryId),
    Goal(GoalId),
}

impl EntityId {
    #[must_use]
    pub const fn uuid(&self) -> Uuid {
        match self {
            Self::Memory(memory_id) => memory_id.into_inner(),
            Self::Goal(goal_id) => goal_id.into_inner(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipRow {
    pub group: GroupId,
    pub relation: Relation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnerRefKind;

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
    fn role_write_is_never_above_read() {
        assert!(Role::new(AccessCeiling::Fact, AccessCeiling::Goal, false).is_err());
        assert!(Role::new(AccessCeiling::Goal, AccessCeiling::Perspective, false).is_ok());
    }

    #[test]
    fn builtin_roles_match_lean_owner_rules() {
        for kind in AccessKind::ALL {
            assert!(Role::personal().may_read(kind));
            assert!(Role::personal().may_write(kind));
            assert!(Role::viewer().may_read(kind));
            assert!(!Role::viewer().may_write(kind));
        }
        assert!(!Role::personal().manages());
        assert!(!Role::viewer().manages());
        assert!(Role::admin().manages());
    }

    #[test]
    fn owner_roles_auto_include_world_and_subject_personal_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let other = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(group);

        let roles = OwnerRoles::for_subject(subject, [(owner, Role::editor())]).unwrap();

        assert!(roles.may_read(&OwnerRef::World, AccessKind::Goal));
        assert!(!roles.may_write(&OwnerRef::World, AccessKind::Fact));
        assert!(!roles.may_manage(&OwnerRef::World));

        assert!(roles.may_write(&OwnerRef::Personal(subject), AccessKind::Goal));
        assert!(!roles.may_manage(&OwnerRef::Personal(subject)));
        assert!(!roles.may_read(&OwnerRef::Personal(other), AccessKind::Fact));

        assert!(roles.may_write(&owner, AccessKind::Perspective));
        assert!(!roles.may_write(&owner, AccessKind::Goal));
    }

    #[test]
    fn owner_roles_reject_world_or_personal_overrides() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        assert!(OwnerRoles::for_subject(subject, [(OwnerRef::World, Role::admin())]).is_err());
        assert!(
            OwnerRoles::for_subject(subject, [(OwnerRef::Personal(subject), Role::admin())])
                .is_err()
        );
    }

    #[test]
    fn relation_lattice_editor_dominates_viewer_only() {
        use Relation::*;
        assert!(Editor.dominates(Viewer));
        assert!(Editor.dominates(Editor));
        assert!(!Editor.dominates(Admin));
        assert!(!Editor.dominates(Ingest));
        assert!(!Viewer.dominates(Editor));
        assert!(Admin.dominates(Admin) && !Admin.dominates(Editor));
        assert!(Ingest.dominates(Ingest) && !Ingest.dominates(Viewer));
    }

    #[test]
    fn world_is_stable_owner_ref() {
        assert_eq!(world(), OwnerRef::World);
    }
}
