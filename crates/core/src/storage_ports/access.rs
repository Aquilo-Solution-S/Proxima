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
    /// Transfer one memory **series** from the permit's owner to `to_owner`.
    /// Same `(handle, t)`; head and every version move together, including
    /// cooled stubs. Returns `true` when a row existed under the permit's
    /// owner and was updated; `false` when no row matched (owner changed
    /// concurrently, or absent) — the caller treats `false` as a clean,
    /// non-panicking denial rather than a storage error. Goal entities are
    /// refused by the engine before this port; implementations fail loudly
    /// on one rather than no-oping.
    ///
    /// **Sidecars co-move, except the owner-pinned ones.** A registered
    /// sidecar keyed by a moved `t` follows the memory, because that is what
    /// keying by `t` means: it is an extra column on the memory and reaches
    /// its owner through it.
    ///
    /// An OWNER-PINNED sidecar carries its own `owner_id`, stamped at write
    /// time with the owner that acted, and does not move. `mcp_call_logged_v1`
    /// is the one core example: it holds `actor_upn`/`actor_oid` and records
    /// who made a tool call rather than what the memory says. This transfer
    /// leaves those rows exactly where they are, and every surface that
    /// reaches them keys on that column rather than on `memory.owner_id`:
    /// the payload hydrate joins the memory's owner to the row's, so
    /// `get_memory`/`get_memories`/`query_memories` at the destination see
    /// nothing; `read_mcp_call_history`, compliance export, and Art. 17
    /// erase all select by the row's own owner, so the source keeps both the
    /// history and the obligation to delete it.
    ///
    /// This replaced a DELETE. Deleting kept the destination out, but it
    /// destroyed audit history the source was entitled to under Art. 15 and
    /// still obliged to erase under Art. 17.
    async fn transfer_to_owner(
        &self,
        permit: &OwnerWritePermit,
        entity: EntityId,
        to_owner: OwnerRef,
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
