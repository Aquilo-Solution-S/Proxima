use std::collections::BTreeSet;

use proxima_core::personality::{
    ListReadScopeRequest, ListReadScopeResponse, PersonalityInstanceId, SetReadScopeRequest,
    SetReadScopeResponse,
};
use proxima_core::{OwnerPrincipalKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

pub async fn list_read_scope(
    pool: &PgPool,
    req: &ListReadScopeRequest,
) -> Result<ListReadScopeResponse, StorageError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, _owner_org_id) = owner.columns();
    ensure_active_personality(
        pool,
        owner_kind,
        owner_principal_id,
        req.reader_personality_instance_id,
    )
    .await?;

    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT readable_personality_instance_id
           FROM proxima_core.read_scope_matrix
	          WHERE owner_principal_kind = $1
	            AND owner_principal_id = $2
	            AND reader_personality_instance_id = $3
	          ORDER BY created_at, readable_personality_instance_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(req.reader_personality_instance_id.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(ListReadScopeResponse {
        readable_personality_instance_ids: rows
            .into_iter()
            .map(|(id,)| PersonalityInstanceId::new(id))
            .collect(),
    })
}

pub async fn set_read_scope(
    pool: &PgPool,
    req: &SetReadScopeRequest,
) -> Result<SetReadScopeResponse, StorageError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let reader_id = req.reader_personality_instance_id.into_inner();
    let readable_ids: Vec<_> = req
        .readable_personality_instance_ids
        .iter()
        .map(|id| id.into_inner())
        .filter(|id| *id != reader_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut tx = pool.begin().await.map_err(map_err)?;
    let reader_exists: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT personality_instance_id
           FROM proxima_core.personality
	          WHERE owner_principal_kind = $1
	            AND owner_principal_id = $2
	            AND personality_instance_id = $3
	            AND status <> 'tombstoned'::proxima_core.personality_status
	          FOR UPDATE",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(reader_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    if reader_exists.is_none() {
        return Err(StorageError::NotFound);
    }

    if !readable_ids.is_empty() {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)
               FROM proxima_core.personality
	              WHERE owner_principal_kind = $1
	                AND owner_principal_id = $2
	                AND status <> 'tombstoned'::proxima_core.personality_status
	                AND personality_instance_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(&readable_ids[..])
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;
        if count != i64::try_from(readable_ids.len()).unwrap_or(i64::MAX) {
            return Err(StorageError::ConstraintViolation(
                "readable personality not found or tombstoned".into(),
            ));
        }
    }

    sqlx::query(
        "DELETE FROM proxima_core.read_scope_matrix
	          WHERE owner_principal_kind = $1
	            AND owner_principal_id = $2
	            AND reader_personality_instance_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(reader_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    for readable_id in &readable_ids {
        sqlx::query(
            "INSERT INTO proxima_core.read_scope_matrix
                (owner_principal_kind, owner_principal_id, owner_org_id,
                 reader_personality_instance_id, readable_personality_instance_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(reader_id)
        .bind(*readable_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    tx.commit().await.map_err(map_err)?;
    Ok(SetReadScopeResponse {
        readable_count: u32::try_from(readable_ids.len()).unwrap_or(u32::MAX),
    })
}

async fn ensure_active_personality(
    pool: &PgPool,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    personality_instance_id: PersonalityInstanceId,
) -> Result<(), StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT personality_instance_id
           FROM proxima_core.personality
	          WHERE owner_principal_kind = $1
	            AND owner_principal_id = $2
	            AND personality_instance_id = $3
	            AND status <> 'tombstoned'::proxima_core.personality_status",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(personality_instance_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    if row.is_some() {
        Ok(())
    } else {
        Err(StorageError::NotFound)
    }
}
