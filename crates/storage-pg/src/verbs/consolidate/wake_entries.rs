use proxima_core::personality::{
    PersonalityInstanceId, PersonalityStatus, SetWakeEntriesRequest, SetWakeEntriesResponse,
    WakeEntryDraft,
};
use proxima_core::{Owner, StorageError};
use sqlx::{PgPool, Row};

use crate::error::map_err;

async fn replace_wake_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let result = sqlx::query(
        "UPDATE proxima_core.personality_wake_entries
         SET tombstoned_at = now(), updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND tombstoned_at IS NULL",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    let _ = result;

    let active_parent = sqlx::query(
        "UPDATE proxima_core.personality
         SET updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if active_parent.rows_affected() == 0 {
        return Err(StorageError::NotFound);
    }

    for entry in &req.entries {
        upsert_wake_entry(tx, &owner, entry).await?;
    }
    Ok(SetWakeEntriesResponse {
        active_entries: u32::try_from(req.entries.len()).unwrap_or(u32::MAX),
    })
}

pub async fn set_wake_entries(
    pool: &PgPool,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let resp = replace_wake_entries_in_tx(&mut tx, req).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(resp)
}

async fn read_wake_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    pid: PersonalityInstanceId,
) -> Result<Vec<WakeEntryDraft>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let rows = sqlx::query(
        "SELECT wake_entry_id, trigger_kind, trigger_id, label, enabled,
                authored_by, probability_promille, goal_scope, instructions
         FROM proxima_core.personality_wake_entries
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND tombstoned_at IS NULL
         ORDER BY label, wake_entry_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(pid.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(WakeEntryDraft {
            wake_entry_id: row.get("wake_entry_id"),
            personality_instance_id: pid,
            trigger_kind: row.get("trigger_kind"),
            trigger_id: row.get("trigger_id"),
            label: row.get("label"),
            enabled: row.get("enabled"),
            authored_by: row.get("authored_by"),
            probability_promille: u16::try_from(row.get::<i32, _>("probability_promille"))
                .unwrap_or(0),
            goal_scope: row.get("goal_scope"),
            instructions: row.get("instructions"),
        });
    }
    Ok(out)
}

pub async fn set_wake_entries_within(
    pool: &PgPool,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    mutate: proxima_core::WakeEntriesMutator,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let mut tx = pool.begin().await.map_err(map_err)?;

    // Lock the personality row to serialise concurrent granular ops.
    let locked: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.personality
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'
         FOR UPDATE",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(personality_instance_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    if locked.is_none() {
        return Err(StorageError::NotFound);
    }

    let current = read_wake_entries_in_tx(&mut tx, owner, personality_instance_id).await?;

    let new_entries = mutate(&current).map_err(StorageError::Internal)?;

    let req = SetWakeEntriesRequest {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        personality_instance_id,
        entries: new_entries,
    };
    let resp = replace_wake_entries_in_tx(&mut tx, &req).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(resp)
}

pub async fn tombstone_personality(
    pool: &PgPool,
    req: &proxima_core::TombstonePersonalityRequest,
) -> Result<proxima_core::TombstonePersonalityResponse, StorageError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let mut tx = pool.begin().await.map_err(map_err)?;

    let result = sqlx::query(
        "UPDATE proxima_core.personality
         SET status = 'tombstoned',
             tombstoned_at = now(),
             updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    if result.rows_affected() == 1 {
        tx.commit().await.map_err(map_err)?;
        return Ok(proxima_core::TombstonePersonalityResponse {
            status: PersonalityStatus::Tombstoned.as_str().into(),
            idempotent_replay: false,
        });
    }

    let existing: Option<(PersonalityStatus,)> = sqlx::query_as(
        "SELECT status
         FROM proxima_core.personality
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;

    match existing {
        Some((PersonalityStatus::Tombstoned,)) => Ok(proxima_core::TombstonePersonalityResponse {
            status: PersonalityStatus::Tombstoned.as_str().into(),
            idempotent_replay: true,
        }),
        Some(_) => {
            unreachable!("UPDATE excluded only tombstoned rows; non-tombstoned must have hit")
        }
        None => Err(StorageError::NotFound),
    }
}

async fn upsert_wake_entry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    entry: &WakeEntryDraft,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_entries
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, trigger_kind, trigger_id,
             label, enabled, authored_by, probability_promille, goal_scope,
             instructions)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 $12, $13)
         ON CONFLICT (
             owner_principal_kind,
             owner_principal_id,
             owner_org_id,
             personality_instance_id,
             wake_entry_id
         ) DO UPDATE SET
             trigger_kind = EXCLUDED.trigger_kind,
             trigger_id = EXCLUDED.trigger_id,
             label = EXCLUDED.label,
             enabled = EXCLUDED.enabled,
             authored_by = EXCLUDED.authored_by,
             probability_promille = EXCLUDED.probability_promille,
             goal_scope = EXCLUDED.goal_scope,
             instructions = EXCLUDED.instructions,
             tombstoned_at = NULL,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(entry.personality_instance_id.into_inner())
    .bind(entry.wake_entry_id)
    .bind(entry.trigger_kind)
    .bind(&entry.trigger_id)
    .bind(&entry.label)
    .bind(entry.enabled)
    .bind(entry.authored_by)
    .bind(i32::from(entry.probability_promille))
    .bind(entry.goal_scope)
    .bind(&entry.instructions)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
