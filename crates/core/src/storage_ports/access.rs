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
    /// **Sidecars co-move, with one deletion.** Every registered sidecar row
    /// keyed by a moved `t` follows the memory, because that is what keying
    /// by `t` means: the generic hydrate path selects `WHERE t = ANY($1)`
    /// with no owner predicate at all, and owner scoping happens only on the
    /// preceding `memory` row query — the column this transfer rewrites.
    /// The audit sidecar `mcp_call_logged_v1` is therefore DELETED rather
    /// than retained: it carries `actor_upn`/`actor_oid`, it describes who
    /// made a tool call rather than the memory, and it has no owner column
    /// of its own to hold it back. Retaining those rows in place would not
    /// keep them at the source — it would publish the prior owner's actor
    /// identities to the destination through `get_memory`, `get_memories`,
    /// `query_memories` (whose `include_payloads` defaults to true), and the
    /// compliance export bundle, while simultaneously moving them out of the
    /// source's own compliance-erase reach (erase selects by
    /// `memory.owner_id`). Genuine retention needs an owner discriminator on
    /// the row plus an owner predicate in the sidecar read path; until that
    /// exists, deletion is the only spelling of "the destination does not
    /// get the source's actors" that the read paths actually enforce.
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
