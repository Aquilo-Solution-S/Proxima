//! Postgres persistence for durable delegated command grants.

use std::time::SystemTime;

use async_trait::async_trait;
use proxima_core::storage_ports::{
    DelegationGrant, DelegationGrantStorage, DelegationId, DelegationMutationPermit,
    DelegationStorePort,
};
use proxima_core::{ActionName, OwnerId, Role, StorageError, UserId};
use sqlx::Row as _;
use time::OffsetDateTime;

use crate::access::owner_columns::PgAccessCeiling;
use crate::error::{internal, map_err};

/// Backend adapter used by the runtime-composed delegated-authority service.
#[doc(hidden)]
#[derive(Clone)]
pub struct PgDelegationStore {
    platform: crate::PgPlatformScope,
}

impl std::fmt::Debug for PgDelegationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PgDelegationStore")
            .finish_non_exhaustive()
    }
}

impl PgDelegationStore {
    #[doc(hidden)]
    #[must_use]
    pub fn new(platform: crate::PgPlatformScope) -> Self {
        Self { platform }
    }
}

#[async_trait]
impl DelegationStorePort for PgDelegationStore {
    async fn insert(
        &self,
        _permit: &DelegationMutationPermit,
        grant: &DelegationGrant,
    ) -> Result<(), StorageError> {
        let owner_id = grant.owner().into_inner();
        let auth_epoch = i64::try_from(grant.auth_epoch()).map_err(|_| {
            StorageError::ConstraintViolation("delegation auth epoch exceeds i64".into())
        })?;
        let mut tx = self.platform.begin().await?;
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                (delegation_id, subject_user_id, owner_id,
                 tool_name, action_name, read_ceiling, write_ceiling,
                 expires_at, auth_epoch, issued_at, \"by\", via)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(grant.delegation_id().into_uuid())
        .bind(grant.subject().into_inner())
        .bind(owner_id)
        .bind(grant.command().tool().as_str())
        .bind(grant.command().action().map(ActionName::as_str))
        .bind(PgAccessCeiling::from(grant.role_ceiling().read_ceiling()))
        .bind(PgAccessCeiling::from(grant.role_ceiling().write_ceiling()))
        .bind(OffsetDateTime::from(grant.expires_at()))
        .bind(auth_epoch)
        .bind(OffsetDateTime::from(grant.issued_at()))
        .bind(grant.writer().by)
        .bind(grant.writer().via)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    }

    async fn load(
        &self,
        permit: &DelegationMutationPermit,
        delegation_id: DelegationId,
        expected_owner: OwnerId,
    ) -> Result<Option<DelegationGrant>, StorageError> {
        let owner_id = expected_owner.into_inner();
        let mut tx = self.platform.begin().await?;
        let row = sqlx::query(
            "SELECT delegation_id, subject_user_id, owner_id,
                    tool_name, action_name, read_ceiling, write_ceiling,
                    expires_at, auth_epoch, issued_at, revoked_at,
                    revoked_by_user_id, \"by\", via
               FROM proxima_core.delegated_authority_grants
              WHERE delegation_id = $1
                AND owner_id IS NOT DISTINCT FROM $2",
        )
        .bind(delegation_id.into_uuid())
        .bind(owner_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        let result = row
            .as_ref()
            .map(|row| decode_grant(permit, row))
            .transpose();
        tx.commit().await.map_err(map_err)?;
        result
    }

    async fn revoke(
        &self,
        _permit: &DelegationMutationPermit,
        delegation_id: DelegationId,
        expected_owner: OwnerId,
        revoked_at: SystemTime,
        revoked_by: UserId,
    ) -> Result<bool, StorageError> {
        let owner_id = expected_owner.into_inner();
        let mut tx = self.platform.begin().await?;
        let result = sqlx::query(
            "UPDATE proxima_core.delegated_authority_grants
                SET revoked_at = $2,
                    revoked_by_user_id = $3
              WHERE delegation_id = $1
                AND owner_id IS NOT DISTINCT FROM $4
                AND revoked_at IS NULL",
        )
        .bind(delegation_id.into_uuid())
        .bind(OffsetDateTime::from(revoked_at))
        .bind(revoked_by.into_inner())
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(result.rows_affected() == 1)
    }
}

fn decode_grant(
    permit: &DelegationMutationPermit,
    row: &sqlx::postgres::PgRow,
) -> Result<DelegationGrant, StorageError> {
    let owner_id: uuid::Uuid = row.try_get("owner_id").map_err(internal)?;
    let owner = OwnerId::new(owner_id);
    let read: PgAccessCeiling = row.try_get("read_ceiling").map_err(internal)?;
    let write: PgAccessCeiling = row.try_get("write_ceiling").map_err(internal)?;
    let role_ceiling = Role::new(read.into(), write.into(), false)
        .map_err(|error| StorageError::Internal(error.to_string()))?;
    let auth_epoch: i64 = row.try_get("auth_epoch").map_err(internal)?;

    DelegationGrant::from_storage(
        permit,
        &DelegationGrantStorage {
            delegation_id: DelegationId::from_uuid(row.try_get("delegation_id").map_err(internal)?),
            subject: UserId::new(row.try_get("subject_user_id").map_err(internal)?),
            owner,
            writer: proxima_core::WriterAttribution {
                by: row.try_get("by").map_err(internal)?,
                via: row.try_get("via").map_err(internal)?,
            },
            tool: row.try_get::<String, _>("tool_name").map_err(internal)?,
            action: row
                .try_get::<Option<String>, _>("action_name")
                .map_err(internal)?,
            role_ceiling,
            expires_at: SystemTime::from(
                row.try_get::<OffsetDateTime, _>("expires_at")
                    .map_err(internal)?,
            ),
            auth_epoch: u64::try_from(auth_epoch)
                .map_err(|_| StorageError::Internal("negative delegation auth epoch".into()))?,
            issued_at: SystemTime::from(
                row.try_get::<OffsetDateTime, _>("issued_at")
                    .map_err(internal)?,
            ),
            revoked_at: row
                .try_get::<Option<OffsetDateTime>, _>("revoked_at")
                .map_err(internal)?
                .map(SystemTime::from),
            revoked_by: row
                .try_get::<Option<uuid::Uuid>, _>("revoked_by_user_id")
                .map_err(internal)?
                .map(UserId::new),
        },
    )
}
