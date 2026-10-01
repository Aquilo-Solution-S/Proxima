//! Owner-role access vocabulary.

use std::collections::HashMap;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::{GoalId, GroupId, MemoryId, OwnerRef, UserId};

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

impl From<crate::EntityKind> for AccessKind {
    fn from(kind: crate::EntityKind) -> Self {
        match kind {
            crate::EntityKind::Fact => Self::Fact,
            crate::EntityKind::Abstraction => Self::Abstraction,
            crate::EntityKind::Perspective => Self::Perspective,
            crate::EntityKind::Goal => Self::Goal,
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

    /// The highest kind this limit covers; `None` covers none.
    #[must_use]
    pub const fn top(self) -> Option<AccessKind> {
        match self {
            Self::None => None,
            Self::Fact => Some(AccessKind::Fact),
            Self::Abstraction => Some(AccessKind::Abstraction),
            Self::Perspective => Some(AccessKind::Perspective),
            Self::Goal => Some(AccessKind::Goal),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AccessError {
    #[error("write ceiling exceeds read ceiling")]
    WriteExceedsRead,
    #[error("personal roles are derived, not resolver-provided")]
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

    /// Owner-level administration — membership admin, transfer, erase, the
    /// graph overview: the full write limit. Membership changes and group
    /// transfers also need [`Self::manages`].
    #[must_use]
    pub const fn administers(self) -> bool {
        self.may_write(AccessKind::Goal)
    }

    #[must_use]
    pub const fn read_ceiling(self) -> AccessCeiling {
        self.read
    }

    #[must_use]
    pub const fn write_ceiling(self) -> AccessCeiling {
        self.write
    }

    /// Greatest role no more powerful than either input.
    ///
    /// Delegated authority uses this as a ceiling: later membership promotion
    /// cannot widen a grant. Redemption separately requires current membership
    /// still to dominate the recorded ceiling, so demotion fails closed.
    #[must_use]
    pub const fn meet(self, other: Self) -> Self {
        let read = if self.read.rank() <= other.read.rank() {
            self.read
        } else {
            other.read
        };
        let write = if self.write.rank() <= other.write.rank() {
            self.write
        } else {
            other.write
        };
        Self {
            read,
            write,
            manage: self.manage && other.manage,
        }
    }

    /// Least role at least as powerful as either input: the stronger read and
    /// write ceilings, and manage when either manages.
    ///
    /// A member who holds two relations in one group holds both, so their role
    /// for that group is the join (`Role.join` in `docs/lean/Causa/Owner.lean`).
    /// `write <= read` holds because each input satisfies it and max is
    /// monotone.
    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        let read = if self.read.rank() >= other.read.rank() {
            self.read
        } else {
            other.read
        };
        let write = if self.write.rank() >= other.write.rank() {
            self.write
        } else {
            other.write
        };
        Self {
            read,
            write,
            manage: self.manage || other.manage,
        }
    }

    /// Whether `self` contains every capability in `required`.
    #[must_use]
    pub const fn dominates(self, required: Self) -> bool {
        self.meet(required).read.rank() == required.read.rank()
            && self.meet(required).write.rank() == required.write.rank()
            && (!required.manage || self.manage)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRoles {
    subject: UserId,
    roles: HashMap<OwnerRef, Role>,
}

impl OwnerRoles {
    /// A Group named more than once (a member holding several relations in
    /// one group) resolves to the [`Role::join`] of its roles, whatever order
    /// the resolver yields them in.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError::DerivedOwnerOverride`] if the resolver tries to
    /// provide Personal roles; those are derived by the kernel rules.
    pub fn for_subject<I>(subject: UserId, group_roles: I) -> Result<Self, AccessError>
    where
        I: IntoIterator<Item = (OwnerRef, Role)>,
    {
        let mut roles = HashMap::new();
        roles.insert(OwnerRef::Personal(subject), Role::personal());
        for (owner, role) in group_roles {
            match owner {
                OwnerRef::Group(_) => {
                    roles
                        .entry(owner)
                        .and_modify(|held: &mut Role| *held = held.join(role))
                        .or_insert(role);
                }
                OwnerRef::Personal(_) => {
                    return Err(AccessError::DerivedOwnerOverride);
                }
            }
        }
        Ok(Self { subject, roles })
    }

    /// The same map with one more host-resolved Group role — the entry the
    /// resolver answered on demand instead of in the eager enumeration.
    /// `role` is the resolver's whole answer for `group` (its relations
    /// already joined, as [`Self::for_subject`] joins them), so it replaces an
    /// earlier entry rather than joining it: a later answer after a demotion is
    /// never widened by the stale one. Typed on [`GroupId`] because Personal
    /// roles are derived by the kernel rules and cannot be resolved, so the
    /// fold has nothing to refuse.
    #[must_use]
    pub fn with_group_role(mut self, group: GroupId, role: Role) -> Self {
        self.roles.insert(OwnerRef::Group(group), role);
        self
    }

    #[must_use]
    pub const fn subject(&self) -> UserId {
        self.subject
    }

    #[must_use]
    pub(crate) fn scoped_to(subject: UserId, owner: OwnerRef, role: Role) -> Self {
        let mut roles = HashMap::new();
        roles.insert(owner, role);
        Self { subject, roles }
    }

    #[must_use]
    pub fn empty_for_subject(subject: UserId) -> Self {
        let mut roles = HashMap::new();
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
            OwnerRef::Personal(_) => false,
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

    /// Resolve only the role `subject` holds in `group`, on demand.
    ///
    /// Same host-resolved currency as [`Self::resolve_roles_for_subject`],
    /// asked one Group at a time. A host that serves many parties from one
    /// trusted forwarder subject cannot bound the eager map — it grows with
    /// the number of Group owners, not with the request — so the edge asks
    /// per selected Group instead. `Ok(None)` is "no role", which every
    /// caller must treat as a refusal; it is never a reason to fall back to
    /// a caller-supplied role.
    ///
    /// Typed on [`GroupId`]: a Personal owner is a kernel rule (the subject's
    /// own is `personal`, anyone else's is no role) and never a resolver's
    /// answer, so it cannot be asked here.
    ///
    /// The default implementation answers out of the eager map, so every
    /// existing implementor keeps its current behavior byte for byte.
    /// Override it when a single-group probe is cheaper than the full
    /// enumeration.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError`] when the host cannot resolve access roles.
    async fn resolve_group_role(
        &self,
        subject: UserId,
        group: GroupId,
    ) -> Result<Option<Role>, AccessError> {
        Ok(self
            .resolve_roles_for_subject(subject)
            .await?
            .role_for(&OwnerRef::Group(group)))
    }
}

/// The stored name of a membership preset. Authorization reads the preset's
/// [`Role`] ([`Self::role`]), never the name.
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
    fn owner_roles_auto_include_subject_personal_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let other = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(group);

        let roles = OwnerRoles::for_subject(subject, [(owner, Role::editor())]).unwrap();

        assert!(roles.may_write(&OwnerRef::Personal(subject), AccessKind::Goal));
        assert!(!roles.may_manage(&OwnerRef::Personal(subject)));
        assert!(!roles.may_read(&OwnerRef::Personal(other), AccessKind::Fact));

        assert!(roles.may_write(&owner, AccessKind::Perspective));
        assert!(!roles.may_write(&owner, AccessKind::Goal));
    }

    #[test]
    fn owner_roles_reject_personal_overrides() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        assert!(
            OwnerRoles::for_subject(subject, [(OwnerRef::Personal(subject), Role::admin())])
                .is_err()
        );
    }

    /// A port that implements only the eager enumeration still answers the
    /// per-group question, through the default method.
    #[tokio::test]
    async fn the_default_per_group_method_answers_out_of_the_eager_map() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let mapped = GroupId::new(uuid::Uuid::now_v7());
        let unmapped = GroupId::new(uuid::Uuid::now_v7());
        let port = EagerOnlyAccess {
            roles: OwnerRoles::for_subject(subject, [(OwnerRef::Group(mapped), Role::editor())])
                .unwrap(),
        };

        assert_eq!(
            port.resolve_group_role(subject, mapped).await.unwrap(),
            Some(Role::editor())
        );
        assert_eq!(
            port.resolve_group_role(subject, unmapped).await.unwrap(),
            None,
            "a group the map lacks is no role, never a default one"
        );
    }

    /// `join` mirrors `Role.join` in `docs/lean/Causa/Owner.lean`: the
    /// stronger ceiling per capability, manage when either manages.
    #[test]
    fn join_is_the_stronger_capability_of_each() {
        let presets = [
            Role::personal(),
            Role::viewer(),
            Role::ingest(),
            Role::editor(),
            Role::admin(),
        ];
        for x in presets {
            for y in presets {
                let joined = x.join(y);
                assert!(joined.dominates(x) && joined.dominates(y));
                assert_eq!(joined, y.join(x));
                assert!(joined.write_ceiling().rank() <= joined.read_ceiling().rank());
            }
        }
        assert_eq!(Role::viewer().join(Role::admin()), Role::admin());
        assert_eq!(Role::editor().join(Role::viewer()), Role::editor());
        let both = Role::viewer().join(Role::ingest());
        assert!(both.may_read(AccessKind::Goal));
        assert!(both.may_write(AccessKind::Fact));
        assert!(!both.may_write(AccessKind::Abstraction));
        assert!(!both.manages());
    }

    /// A member holding several relations in one group resolves to their
    /// join, in whichever order the resolver yields them. The storage
    /// resolver yields them in enum order, `admin` first: before the join, the
    /// last and weakest row won.
    #[test]
    fn several_relations_in_one_group_resolve_to_their_join() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        for rows in [
            [Role::admin(), Role::viewer()],
            [Role::viewer(), Role::admin()],
        ] {
            let roles = OwnerRoles::for_subject(subject, rows.map(|role| (owner, role))).unwrap();
            assert_eq!(roles.role_for(&owner), Some(Role::admin()));
        }
        let roles =
            OwnerRoles::for_subject(subject, [(owner, Role::ingest()), (owner, Role::viewer())])
                .unwrap();
        assert_eq!(
            roles.role_for(&owner),
            Some(Role::viewer().join(Role::ingest()))
        );
    }

    /// The entry lands in the map, a later whole answer replaces an earlier
    /// one, and the derived personal entry is untouched.
    #[test]
    fn a_group_role_folds_into_the_map() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let roles = OwnerRoles::for_subject(subject, [])
            .unwrap()
            .with_group_role(group, Role::viewer())
            .with_group_role(group, Role::editor());
        assert_eq!(
            roles.role_for(&OwnerRef::Group(group)),
            Some(Role::editor())
        );
        assert_eq!(
            roles.role_for(&OwnerRef::Personal(subject)),
            Some(Role::personal())
        );
        let demoted = roles.with_group_role(group, Role::viewer());
        assert_eq!(
            demoted.role_for(&OwnerRef::Group(group)),
            Some(Role::viewer()),
            "a later answer after a demotion is not widened by the stale one"
        );
    }

    /// Implements the eager method only: the per-group method it inherits is
    /// exactly the default under test.
    struct EagerOnlyAccess {
        roles: OwnerRoles,
    }

    #[async_trait]
    impl OwnerAccessPort for EagerOnlyAccess {
        async fn resolve_roles_for_subject(
            &self,
            _subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            Ok(self.roles.clone())
        }
    }
}
