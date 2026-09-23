use proxima_core::storage_ports::{
    OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort, OwnerWritePermit,
    SourceCursorPort,
};

use proxima_core::{
    Cursor, EntityId, GroupId, MembershipRow, Owner, OwnerRef, Relation, StorageError, UserId,
};

use super::validate_permit_owner;
use crate::{PgStorage, access, verbs};

#[async_trait::async_trait]
impl OwnerAccessReadPort for PgStorage {
    async fn resolve_membership(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        if let Some(scope) = owner_scope {
            if scope.is_expired() || *member != OwnerRef::Personal(scope.subject()) {
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
        read_owners: &[OwnerRef],
    ) -> Result<Option<OwnerRef>, StorageError> {
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
    ) -> Result<Option<OwnerRef>, StorageError> {
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
        permit: &OwnerWritePermit,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        validate_permit_owner(permit, &OwnerRef::Group(group_id))?;
        access::owner_columns::add_group_member(
            &self.pool,
            permit.owner_scope(),
            group_id,
            member_user_id,
            relation,
            granted_by,
        )
        .await
    }

    async fn remove_group_member(
        &self,
        permit: &OwnerWritePermit,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError> {
        validate_permit_owner(permit, &OwnerRef::Group(group_id))?;
        access::owner_columns::remove_group_member(
            &self.pool,
            permit.owner_scope(),
            group_id,
            member_user_id,
        )
        .await
    }

    async fn list_group_members(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = access::owner_columns::list_group_members(tx.as_mut(), group_id).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn list_group_members_page(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        group_id: GroupId,
        after: Option<(UserId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
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
        to_owner: OwnerRef,
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
        owner: &Owner,
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
        owner: &Owner,
        source: &str,
    ) -> Result<Option<std::time::Duration>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::source_cursors::source_cursor_age(tx.as_mut(), owner, source).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
