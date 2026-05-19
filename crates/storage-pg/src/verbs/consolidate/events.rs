use proxima_core::personality::{ChangeEventForWake, PersonalityInstanceId, WakeChainDepth};
use proxima_core::{Owner, OwnerPrincipalKind, StorageError};
use sqlx::PgPool;

use super::rows::owner_columns;
use crate::error::map_err;
use crate::outbox::hydrate_change_event;

pub async fn list_change_events_after(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows = sqlx::query!(
        r#"SELECT seq, entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND seq > $4
             ORDER BY seq ASC
             LIMIT $5"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        after,
        i64::try_from(limit).unwrap_or(i64::MAX),
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if let Some(event) = hydrate_change_event(pool, r.seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: r
                    .entity_personality_instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(
                    u16::try_from(r.wake_chain_depth).unwrap_or(0),
                ),
            });
        }
    }
    Ok(out)
}

pub async fn list_change_events_for_replay(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows = sqlx::query!(
        r#"SELECT seq, entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND seq > $4
               AND ($5::uuid IS NULL OR seq <= $5)
             ORDER BY seq ASC
             LIMIT $6"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        after,
        until,
        i64::try_from(limit).unwrap_or(i64::MAX),
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if let Some(event) = hydrate_change_event(pool, r.seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: r
                    .entity_personality_instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(
                    u16::try_from(r.wake_chain_depth).unwrap_or(0),
                ),
            });
        }
    }
    Ok(out)
}
