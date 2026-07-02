use crate::storage::StorageError;
use crate::{EntityId, GroupId, MembershipRow, OwnerRef, Relation, UserId};

#[async_trait::async_trait]
pub trait OwnerAccessReadPort: Send + Sync {
    async fn resolve_membership(
        &self,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    async fn visible_to_any(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError>;

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError>;
}

#[async_trait::async_trait]
pub trait OwnerTransferPort: Send + Sync {
    /// Transfer one memory or goal row's owner columns to
    /// [`OwnerRef::World`] in a single statement, gated on the row
    /// currently being owned by `from_owner`. Returns `true` when a row
    /// existed under `from_owner` and was updated; `false` when no row
    /// matched (already published, owner changed concurrently, tombstoned,
    /// or absent) — the caller treats `false` as a clean, non-panicking
    /// denial rather than a storage error.
    async fn transfer_to_world(
        &self,
        entity: EntityId,
        from_owner: OwnerRef,
    ) -> Result<bool, StorageError>;
}

#[async_trait::async_trait]
pub trait OwnerMembershipAdminPort: Send + Sync {
    async fn bootstrap_group_admin(
        &self,
        group_id: GroupId,
        first_admin_user_id: UserId,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn add_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn remove_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError>;

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;
}
