use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{EntityId, GroupId, MembershipRow, OwnerRef, Relation, UserId};

#[async_trait::async_trait]
pub trait OwnerAccessReadPort: Send + Sync {
    async fn resolve_membership(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    /// Home owner and kind of `entity` when its owner is in `read_owners`.
    /// Absent and foreign are both `None`.
    async fn visible_home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<Option<(OwnerRef, crate::AccessKind)>, StorageError>;

    async fn home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        entity: EntityId,
    ) -> Result<Option<OwnerRef>, StorageError>;
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
    /// nothing; `read_mcp_call_history`, the owner export and the owner erase
    /// all select by the row's own owner, so the source keeps both the history
    /// and the ability to destroy it.
    ///
    /// The rows stay put rather than being deleted. Deleting would keep the
    /// destination out, but it would also destroy history the source could
    /// still be asked to produce and leave nothing for the source's own
    /// erase to reach: rows a host cannot export are an inconvenience, rows
    /// a host cannot destroy are a promise it cannot keep.
    ///
    /// `surfaces` carries the registry-resolved [`TransferLeg`] per table.
    /// The verb READS those answers; it does not re-derive them and holds no
    /// table list of its own, exactly as erase and export do.
    ///
    /// `embedding_spaces` are the spaces the destination's route names: the
    /// moved series' vectors, heads and jobs in any other space are deleted
    /// in the transfer, and its head is queued for each of them it has no
    /// vector in.
    ///
    /// [`TransferLeg`]: crate::flavor::TransferLeg
    async fn transfer_to_owner(
        &self,
        permit: &OwnerWritePermit,
        entity: EntityId,
        to_owner: OwnerRef,
        surfaces: &crate::owner_inverse::OwnerSurfaces,
        embedding_spaces: &[crate::EmbeddingSpace],
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

    /// Remove the one `relation` row `member_user_id` holds in `group_id`;
    /// the member's other relations stay. An absent row is `Ok`, as
    /// [`Self::remove_group_member`] is `Ok` for a member with no rows.
    ///
    /// The default refuses, so an implementation written before this method
    /// existed still compiles and refuses the call.
    async fn remove_group_member_relation(
        &self,
        _permit: &OwnerWritePermit,
        _group_id: GroupId,
        _member_user_id: UserId,
        _relation: Relation,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "OwnerMembershipAdminPort::remove_group_member_relation is not implemented by this storage"
                .into(),
        ))
    }

    /// Replace the `from` relation `member_user_id` holds in `group_id` with
    /// `to`, atomically: the one `from` row is deleted and the `to` row
    /// inserted (an existing `to` row is kept) in one transaction under the
    /// group's membership lock, so no reader sees the member with neither
    /// role. The member's other relations stay.
    ///
    /// # Errors
    ///
    /// Returns `Conflict`, with nothing changed, when the member does not hold
    /// `from`. The engine refuses `from == to` before this port; a direct
    /// caller gets the same transaction, so `from` must still be held.
    ///
    /// The default refuses, so an implementation written before this method
    /// existed still compiles and refuses the call.
    async fn replace_group_member_relation(
        &self,
        _permit: &OwnerWritePermit,
        _group_id: GroupId,
        _member_user_id: UserId,
        _from: Relation,
        _to: Relation,
        _granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "OwnerMembershipAdminPort::replace_group_member_relation is not implemented by this storage"
                .into(),
        ))
    }

    async fn list_group_members(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;

    /// One page of group members in the keyset total order
    /// `(member_user_id, relation)`, starting strictly after `after` when
    /// given. Callers over-fetch by one to detect further pages.
    async fn list_group_members_page(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        group_id: GroupId,
        after: Option<(UserId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::OwnerMembershipAdminPort;
    use crate::storage_ports::OwnerWritePermit;
    use crate::{AccessKind, GroupId, OwnerRef, Relation, StorageError, UserId};
    use uuid::Uuid;

    /// A port written before the relation-level methods existed: it supplies
    /// only the original required methods.
    struct LegacyMembershipAdmin;

    #[async_trait::async_trait]
    impl OwnerMembershipAdminPort for LegacyMembershipAdmin {
        async fn bootstrap_group_admin(
            &self,
            _group_id: GroupId,
            _first_admin_user_id: UserId,
            _granted_by: Uuid,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn add_group_member(
            &self,
            _permit: &OwnerWritePermit,
            _group_id: GroupId,
            _member_user_id: UserId,
            _relation: Relation,
            _granted_by: Uuid,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn remove_group_member(
            &self,
            _permit: &OwnerWritePermit,
            _group_id: GroupId,
            _member_user_id: UserId,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_group_members(
            &self,
            _owner_scope: Option<&crate::OwnerScope>,
            _group_id: GroupId,
        ) -> Result<Vec<(UserId, Relation)>, StorageError> {
            Ok(Vec::new())
        }

        async fn list_group_members_page(
            &self,
            _owner_scope: Option<&crate::OwnerScope>,
            _group_id: GroupId,
            _after: Option<(UserId, Relation)>,
            _limit: i64,
        ) -> Result<Vec<(UserId, Relation)>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn a_port_without_the_relation_methods_refuses_them_by_name() {
        let group = GroupId::new(Uuid::now_v7());
        let member = UserId::new(Uuid::now_v7());
        let permit = OwnerWritePermit::new_for_tests(OwnerRef::Group(group), AccessKind::Goal);
        let port = LegacyMembershipAdmin;

        let removed = port
            .remove_group_member_relation(&permit, group, member, Relation::Viewer)
            .await
            .expect_err("the default must refuse");
        let replaced = port
            .replace_group_member_relation(
                &permit,
                group,
                member,
                Relation::Admin,
                Relation::Viewer,
                Uuid::now_v7(),
            )
            .await
            .expect_err("the default must refuse");

        assert!(
            matches!(&removed, StorageError::Internal(message)
                if message.contains("remove_group_member_relation")),
            "refusal must name the missing method: {removed:?}"
        );
        assert!(
            matches!(&replaced, StorageError::Internal(message)
                if message.contains("replace_group_member_relation")),
            "refusal must name the missing method: {replaced:?}"
        );
    }
}
