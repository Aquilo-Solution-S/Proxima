use crate::storage::StorageError;
use crate::storage_ports::{MembershipWritePermit, OwnerWritePermit};
use crate::{EntityId, GroupId, MembershipRow, OwnerId, Relation, UserId};

#[async_trait::async_trait]
pub trait OwnerAccessReadPort: Send + Sync {
    async fn resolve_membership(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        member: &OwnerId,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    /// Home owner and kind of `entity` when its owner is in `read_owners`.
    /// Absent and foreign are both `None`.
    async fn visible_home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        entity: EntityId,
        read_owners: &[OwnerId],
    ) -> Result<Option<(OwnerId, crate::AccessKind)>, StorageError>;

    async fn home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        entity: EntityId,
    ) -> Result<Option<OwnerId>, StorageError>;
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
    /// time with the owner that acted, and does not move. The registry-resolved
    /// transfer leg determines how such a sidecar is retained and erased.
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
        to_owner: OwnerId,
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
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member_id: OwnerId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn remove_group_member(
        &self,
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member_id: OwnerId,
    ) -> Result<(), StorageError>;

    /// Remove the one `relation` row `member_user_id` holds in `group_id`;
    /// the member's other relations stay. An absent row is `Ok`, as
    /// [`Self::remove_group_member`] is `Ok` for a member with no rows.
    ///
    /// The default refuses, so an implementation written before this method
    /// existed still compiles and refuses the call.
    async fn remove_group_member_relation(
        &self,
        _permit: &MembershipWritePermit<'_>,
        _group_id: GroupId,
        _member_id: OwnerId,
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
        _permit: &MembershipWritePermit<'_>,
        _group_id: GroupId,
        _member_id: OwnerId,
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
    ) -> Result<Vec<(OwnerId, Relation)>, StorageError>;

    /// One page of group members in the keyset total order
    /// `(member_user_id, relation)`, starting strictly after `after` when
    /// given. Callers over-fetch by one to detect further pages.
    async fn list_group_members_page(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        group_id: GroupId,
        after: Option<(OwnerId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(OwnerId, Relation)>, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::OwnerMembershipAdminPort;
    use crate::storage_ports::{
        MembershipMemberAuthority, MembershipWritePermit, OwnerWritePermit,
    };
    use crate::{AccessKind, GroupId, OwnerId, Relation, StorageError, UserId};
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
            _permit: &MembershipWritePermit<'_>,
            _group_id: GroupId,
            _member_id: OwnerId,
            _relation: Relation,
            _granted_by: Uuid,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn remove_group_member(
            &self,
            _permit: &MembershipWritePermit<'_>,
            _group_id: GroupId,
            _member_id: OwnerId,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_group_members(
            &self,
            _owner_scope: Option<&crate::OwnerScope>,
            _group_id: GroupId,
        ) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
            Ok(Vec::new())
        }

        async fn list_group_members_page(
            &self,
            _owner_scope: Option<&crate::OwnerScope>,
            _group_id: GroupId,
            _after: Option<(OwnerId, Relation)>,
            _limit: i64,
        ) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn a_port_without_the_relation_methods_refuses_them_by_name() {
        let group = GroupId::new(Uuid::now_v7());
        let member = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(OwnerId::from(group), AccessKind::Goal);
        let membership_permit = MembershipWritePermit::new(
            &permit,
            OwnerId::from(group),
            MembershipMemberAuthority::CallerDoesNotManageMember,
            crate::storage_ports::MembershipEndpointAuthority::Parent,
        );
        let port = LegacyMembershipAdmin;

        let removed = port
            .remove_group_member_relation(&membership_permit, group, member, Relation::Viewer)
            .await
            .expect_err("the default must refuse");
        let replaced = port
            .replace_group_member_relation(
                &membership_permit,
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
