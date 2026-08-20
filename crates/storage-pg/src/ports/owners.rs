use proxima_core::storage_ports::{
    FactRetentionPort, OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort,
    OwnerWritePermit, SourceBatchPort, SourceCursorPort,
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
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        access::owner_columns::resolve_membership(&self.pool, member).await
    }

    async fn visible_home_owner(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<Option<OwnerRef>, StorageError> {
        access::owner_columns::visible_home_owner(&self.pool, entity, read_owners).await
    }

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
        access::owner_columns::home_owner(&self.pool, entity).await
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
        access::owner_columns::remove_group_member(&self.pool, group_id, member_user_id).await
    }

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        access::owner_columns::list_group_members(&self.pool, group_id).await
    }

    async fn list_group_members_page(
        &self,
        group_id: GroupId,
        after: Option<(UserId, Relation)>,
        limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        access::owner_columns::list_group_members_page(&self.pool, group_id, after, limit).await
    }
}

#[async_trait::async_trait]
impl OwnerTransferPort for PgStorage {
    async fn transfer_to_owner(
        &self,
        permit: &OwnerWritePermit,
        entity: EntityId,
        to_owner: OwnerRef,
    ) -> Result<bool, StorageError> {
        access::owner_columns::transfer_to_owner(&self.pool, entity, *permit.owner(), to_owner)
            .await
    }
}

impl SourceBatchPort for PgStorage {}

#[async_trait::async_trait]
impl SourceCursorPort for PgStorage {
    async fn load_source_cursor(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Cursor>, StorageError> {
        verbs::source_cursors::load_source_cursor(&self.pool, owner, source).await
    }

    async fn store_source_cursor(
        &self,
        permit: &OwnerWritePermit,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), StorageError> {
        verbs::source_cursors::store_source_cursor(&self.pool, permit, source, cursor).await
    }

    async fn source_cursor_age(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<std::time::Duration>, StorageError> {
        verbs::source_cursors::source_cursor_age(&self.pool, owner, source).await
    }
}

#[async_trait::async_trait]
impl FactRetentionPort for PgStorage {
    async fn upsert_fact_retention(
        &self,
        permit: &OwnerWritePermit,
        seconds: i64,
    ) -> Result<(), StorageError> {
        verbs::fact_retention::upsert_fact_retention(&self.pool, permit, seconds).await
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        verbs::fact_retention::get_fact_retention(&self.pool, owner).await
    }

    async fn clear_fact_retention(&self, permit: &OwnerWritePermit) -> Result<bool, StorageError> {
        verbs::fact_retention::clear_fact_retention(&self.pool, permit).await
    }

    async fn set_legal_hold(&self, permit: &OwnerWritePermit) -> Result<(), StorageError> {
        verbs::fact_retention::set_legal_hold(&self.pool, permit).await
    }

    async fn get_legal_hold(&self, owner: &Owner) -> Result<bool, StorageError> {
        verbs::fact_retention::get_legal_hold(&self.pool, owner).await
    }

    async fn clear_legal_hold(&self, permit: &OwnerWritePermit) -> Result<bool, StorageError> {
        verbs::fact_retention::clear_legal_hold(&self.pool, permit).await
    }
}
