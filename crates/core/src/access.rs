//! Group-ownership access vocabulary.

use crate::{GoalId, GroupId, MemoryId, Principal};
use uuid::Uuid;

pub const WORLD_GROUP_ID: GroupId = GroupId::new(Uuid::from_u128(1));

#[must_use]
pub fn world() -> Principal {
    Principal::Group(WORLD_GROUP_ID)
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
}

/// The sole surviving per-token memory capability after the `RoleSet`/grant
/// collapse: whether the caller bypasses persisted grants entirely, or is
/// decided purely by storage-backed access predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessScope {
    /// Acts as permitted by identity-owned/unrestricted auth context.
    Unrestricted,
    /// Access decided solely by persisted ownership/membership predicates.
    Granted,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityOwnerRow {
    pub owner: Principal,
    pub is_home: bool,
}

/// Result of owner removal — the last-owner orphan guard makes this a
/// three-way outcome rather than a row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOwnerOutcome {
    Removed,
    /// Refused: removing this owner would leave the entity ownerless.
    RefusedLastOwner,
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn world_is_a_group_constant() {
        assert!(
            matches!(crate::access::world(), crate::Principal::Group(g) if g == crate::access::WORLD_GROUP_ID)
        );
    }
}
