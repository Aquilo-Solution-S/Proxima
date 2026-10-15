use proxima_core::storage_ports::{
    MembershipEndpointAuthority, MembershipMemberAuthority, MembershipWritePermit,
    OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort, OwnerWritePermit,
    SourceCursorPort,
};

use proxima_core::{
    Cursor, EntityId, GroupId, MembershipRow, OwnerId, Relation, StorageError, UserId,
};

use super::validate_permit_owner;

fn validate_membership_permit(
    permit: &MembershipWritePermit<'_>,
    group: GroupId,
    member: OwnerId,
) -> Result<(), StorageError> {
    match permit.endpoint_authority() {
        MembershipEndpointAuthority::Parent => {
            validate_permit_owner(permit.owner(), &OwnerId::from(group))?;
        }
        MembershipEndpointAuthority::Member => {
            if permit.member_authority() != MembershipMemberAuthority::CallerManagesMember
                || permit.owner().owner() != &member
            {
                return Err(StorageError::MembershipForbidden(
                    "membership permit does not authorize the attached owner".into(),
                ));
            }
        }
    }
    if permit.parent() != OwnerId::from(group) {
        return Err(StorageError::ConstraintViolation(
            "membership permit parent does not match the requested group".into(),
        ));
    }
    Ok(())
}

fn validate_parent_membership_permit(
    permit: &MembershipWritePermit<'_>,
    group: GroupId,
) -> Result<(), StorageError> {
    if permit.endpoint_authority() != MembershipEndpointAuthority::Parent {
        return Err(StorageError::ConstraintViolation(
            "this membership operation requires parent authority".into(),
        ));
    }
    validate_permit_owner(permit.owner(), &OwnerId::from(group))?;
    if permit.parent() != OwnerId::from(group) {
        return Err(StorageError::ConstraintViolation(
            "membership permit parent does not match the requested group".into(),
        ));
    }
    Ok(())
}
use crate::{PgStorage, access, verbs};

#[async_trait::async_trait]
impl OwnerAccessReadPort for PgStorage {
    async fn resolve_membership(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        member: &OwnerId,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        if let Some(scope) = owner_scope {
            if scope.is_expired() || *member != OwnerId::from(scope.subject()) {
                return Err(StorageError::ConstraintViolation(
                    "membership lookup requires its authenticated subject".into(),
                ));
            }
        } else {
            // Refuse missing authority after activation before opening platform access.
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, None)
                .await?
                .rollback()
                .await
                .map_err(crate::error::map_err)?;
        }
        let mut tx = self.platform_transaction().await?;
        let result = access::owner_columns::resolve_membership(tx.as_mut(), member).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn visible_home_owner(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        entity: EntityId,
        read_owners: &[OwnerId],
    ) -> Result<Option<(OwnerId, proxima_core::AccessKind)>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result =
            access::owner_columns::visible_home_owner(tx.as_mut(), entity, read_owners).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn home_owner(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        entity: EntityId,
    ) -> Result<Option<OwnerId>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = access::owner_columns::home_owner(tx.as_mut(), entity).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl OwnerMembershipAdminPort for PgStorage {
    async fn bootstrap_group_admin(
        &self,
        group_id: GroupId,
        first_admin_user_id: UserId,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        access::owner_columns::bootstrap_group_admin(
            &self.pool,
            self.platform_scope.as_ref(),
            group_id,
            first_admin_user_id,
            granted_by,
        )
        .await
    }

    async fn add_group_member(
        &self,
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member: OwnerId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        validate_parent_membership_permit(permit, group_id)?;
        access::owner_columns::add_group_member(
            &self.pool,
            permit.owner().owner_scope(),
            group_id,
            member,
            relation,
            permit.member_authority(),
            granted_by,
        )
        .await
    }

    async fn remove_group_member(
        &self,
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member: OwnerId,
    ) -> Result<(), StorageError> {
        validate_membership_permit(permit, group_id, member)?;
        access::owner_columns::remove_group_member(
            &self.pool,
            permit.owner().owner_scope(),
            group_id,
            member,
            permit.member_authority(),
            permit.endpoint_authority(),
        )
        .await
    }

    async fn remove_group_member_relation(
        &self,
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member: OwnerId,
        relation: Relation,
    ) -> Result<(), StorageError> {
        validate_membership_permit(permit, group_id, member)?;
        access::owner_columns::remove_group_member_relation(
            &self.pool,
            permit.owner().owner_scope(),
            group_id,
            member,
            relation,
            permit.member_authority(),
            permit.endpoint_authority(),
        )
        .await
    }

    async fn replace_group_member_relation(
        &self,
        permit: &MembershipWritePermit<'_>,
        group_id: GroupId,
        member: OwnerId,
        from: Relation,
        to: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        validate_parent_membership_permit(permit, group_id)?;
        access::owner_columns::replace_group_member_relation(
            &self.pool,
            permit.owner().owner_scope(),
            group_id,
            member,
            from,
            to,
            granted_by,
        )
        .await
    }

    async fn list_group_members(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        group_id: GroupId,
    ) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = access::owner_columns::list_group_members(tx.as_mut(), group_id).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn list_group_members_page(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        group_id: GroupId,
        after: Option<(OwnerId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(OwnerId, Relation)>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result =
            access::owner_columns::list_group_members_page(tx.as_mut(), group_id, after, limit)
                .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl OwnerTransferPort for PgStorage {
    async fn transfer_to_owner(
        &self,
        permit: &OwnerWritePermit,
        entity: EntityId,
        to_owner: OwnerId,
        surfaces: &proxima_core::owner_inverse::OwnerSurfaces,
        embedding_spaces: &[proxima_core::EmbeddingSpace],
    ) -> Result<bool, StorageError> {
        if permit.owner_scope().is_some() && permit.transfer_destination() != Some(to_owner) {
            return Err(StorageError::ConstraintViolation(
                "transfer destination lacks engine consent".into(),
            ));
        }
        access::owner_columns::transfer_to_owner(
            &self.pool,
            self.platform_scope.as_ref(),
            permit,
            surfaces,
            entity,
            to_owner,
            verbs::fact_embeddings::RouteSpaces {
                spaces: embedding_spaces,
                non_embeddable_schemas: &self.non_embeddable_schemas,
            },
        )
        .await
    }
}

#[async_trait::async_trait]
impl SourceCursorPort for PgStorage {
    async fn load_source_cursor(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        source: &str,
    ) -> Result<Option<Cursor>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::source_cursors::load_source_cursor(tx.as_mut(), owner, source).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn store_source_cursor(
        &self,
        permit: &OwnerWritePermit,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), StorageError> {
        let mut tx = crate::owner_scope::begin_compatible_owner_transaction(
            &self.pool,
            permit.owner_scope(),
        )
        .await?;
        let result =
            verbs::source_cursors::store_source_cursor(tx.as_mut(), permit, source, cursor).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn source_cursor_age(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        source: &str,
    ) -> Result<Option<std::time::Duration>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::source_cursors::source_cursor_age(tx.as_mut(), owner, source).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
