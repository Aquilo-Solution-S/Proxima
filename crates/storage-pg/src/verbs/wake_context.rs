//! Phase 1d: thin SELECTs that feed `assemble_wake_context` in
//! `proxima_core::wake::context`. Each function maps 1:1 onto a
//! `Storage` trait method.

use proxima_core::personality::{
    ChangeEventForWake, PersonalityInstanceId, PersonalityRuntimeRow, PersonalityStatus,
    RootPersonalityPerspectiveRow, WakeChainDepth,
};
use proxima_core::{MemoryId, Owner, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::outbox::hydrate_change_event;

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

pub(crate) async fn fetch_personality_runtime(
    pool: &PgPool,
    owner: &Owner,
    instance_id: PersonalityInstanceId,
) -> Result<Option<PersonalityRuntimeRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    // The Root-Perspective memory row carries the wake display text;
    // the instance id lives on `proxima_core.personality`.
    let row = sqlx::query!(
        r#"SELECT p.current_root_perspective_memory_id,
                  m.text AS display_name,
                  p.status AS "status: PersonalityStatus"
             FROM proxima_core.personality p
             JOIN proxima_core.memories m
               ON m.memory_id = p.current_root_perspective_memory_id
             WHERE p.owner_principal_kind = $1
               AND p.owner_principal_id = $2
               AND p.owner_org_id = $3
               AND p.personality_instance_id = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        instance_id.into_inner(),
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    Ok(row.map(|r| PersonalityRuntimeRow {
        owner: owner.clone(),
        personality_instance_id: instance_id,
        current_root_perspective_memory_id: MemoryId::new(r.current_root_perspective_memory_id),
        display_name: r.display_name.unwrap_or_default(),
        status: r.status,
    }))
}

pub(crate) async fn fetch_root_personality_perspective(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    // Owner-scope is enforced by joining the memories row first; the
    // sidecar table is keyed only on memory_id.
    let row = sqlx::query!(
        r#"SELECT s.display_name, s.purpose
             FROM proxima_core.root_personality_perspective_v1 s
             JOIN proxima_core.memories m
               ON m.memory_id = s.memory_id
             WHERE s.memory_id = $1
               AND m.owner_principal_kind = $2
               AND m.owner_principal_id = $3
               AND m.owner_org_id = $4"#,
        memory_id.into_inner(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    Ok(row.map(|r| RootPersonalityPerspectiveRow {
        memory_id,
        display_name: r.display_name,
        purpose: r.purpose,
    }))
}

pub(crate) async fn fetch_change_event_for_wake(
    pool: &PgPool,
    owner: &Owner,
    seq: Uuid,
) -> Result<Option<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let row = sqlx::query!(
        r#"SELECT entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND seq = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        seq,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    let Some(row) = row else {
        return Ok(None);
    };
    let Some(event) = hydrate_change_event(pool, seq).await? else {
        return Ok(None);
    };
    Ok(Some(ChangeEventForWake {
        event,
        authoring_personality_instance_id: row
            .entity_personality_instance_id
            .filter(|id| !id.is_nil())
            .map(PersonalityInstanceId::new),
        wake_chain_depth: WakeChainDepth::new(u16::try_from(row.wake_chain_depth).unwrap_or(0)),
    }))
}
