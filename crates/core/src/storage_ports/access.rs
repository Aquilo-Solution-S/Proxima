use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{EntityId, GroupId, MembershipRow, OwnerRef, Relation, UserId};

#[async_trait::async_trait]
pub trait OwnerAccessReadPort: Send + Sync {
    async fn resolve_membership(
        &self,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    async fn visible_home_owner(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<Option<OwnerRef>, StorageError>;

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError>;
}

#[async_trait::async_trait]
pub trait OwnerTransferPort: Send + Sync {
    /// Transfer one memory or goal **series** to [`OwnerRef::World`].
    /// Same `(handle, t)`; head and every version move together, including
    /// cooled stubs. Returns
    /// `true` when a row existed under `from_owner` and was updated;
    /// `false` when no row matched (already published, owner changed
    /// concurrently, or absent) — the caller treats `false` as a clean,
    /// non-panicking denial rather than a storage error.
    async fn transfer_to_world(
        &self,
        permit: &OwnerWritePermit,
        entity: EntityId,
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
        permit: &OwnerWritePermit,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn remove_group_member(
        &self,
        permit: &OwnerWritePermit,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError>;

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;

    /// One page of group members in the keyset total order
    /// `(member_user_id, relation)`, starting strictly after `after` when
    /// given. Callers over-fetch by one to detect further pages.
    async fn list_group_members_page(
        &self,
        group_id: GroupId,
        after: Option<(UserId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;
}
