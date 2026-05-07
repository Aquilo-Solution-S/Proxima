//! Personality wake/decide/write storage helpers.

use proxima_core::personality::{
    AbstractionRow, ChangeEventForWake, FactRow, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, MemorySnapshot, PersonalityInstanceId, PersonalityInstanceRow,
    PersonalityRef, PersonalityWriteOutcome, PersonalityWriteRequest, SetWakeConfigRequest,
    SetWakeConfigResponse, SidecarSpec, WakeChainDepth, WakeConfigRow, WakeInvocationStatus,
};
use proxima_core::{MemoryId, Owner, Principal, SchemaId, SchemaVersion, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::outbox::hydrate_change_event;
use crate::pg_ident::PgIdent;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

const EXTERNAL_PERSONALITY_TYPE_ID: &str = "external/event-source";
const EXTERNAL_PERSONALITY_INSTANCE_ID: uuid::Uuid = uuid::Uuid::nil();

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

pub async fn list_personality_instances(
    pool: &PgPool,
    owner: &Owner,
    personality_type_id: Option<&str>,
    include_tombstoned: bool,
) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(
        String,
        uuid::Uuid,
        uuid::Uuid,
        String,
        serde_json::Value,
        String,
    )> = sqlx::query_as(
        "SELECT c.personality_type_id,
                c.personality_instance_id,
                c.current_self_perspective_memory_id,
                m.text AS display_name,
                c.wake_filters,
                c.status
         FROM proxima_core.personality_wake_config c
         JOIN proxima_core.memories m
           ON m.memory_id = c.current_self_perspective_memory_id
         WHERE c.owner_principal_kind = $1
           AND c.owner_principal_id = $2
           AND c.owner_org_id = $3
           AND ($4::text IS NULL OR c.personality_type_id = $4)
           AND ($5::bool OR c.status <> 'tombstoned')
         ORDER BY c.personality_type_id, c.created_at, c.personality_instance_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(personality_type_id)
    .bind(include_tombstoned)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    rows.into_iter()
        .map(
            |(
                personality_type_id,
                instance_id,
                self_memory_id,
                display_name,
                filters_json,
                status,
            )| {
                let wake_filters = serde_json::from_value(filters_json)
                    .map_err(|e| StorageError::Internal(format!("wake_filters decode: {e}")))?;
                Ok(PersonalityInstanceRow {
                    owner: owner.clone(),
                    personality_type_id,
                    personality_instance_id: PersonalityInstanceId::new(instance_id),
                    current_self_perspective_memory_id: MemoryId::new(self_memory_id),
                    display_name,
                    status,
                    wake_filters,
                })
            },
        )
        .collect()
}

pub async fn instantiate_personality(
    pool: &PgPool,
    req: &InstantiatePersonalityRequest,
    self_draft: &proxima_core::PersonalitySelfDraft,
    self_sidecar_table: &str,
    default_wake_filters: &[proxima_core::WakeFilter],
) -> Result<InstantiatePersonalityResponse, StorageError> {
    let self_sidecar = PgIdent::table(self_sidecar_table)?;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let instance_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let filters_json = serde_json::to_value(default_wake_filters)
        .map_err(|e| StorageError::Internal(format!("wake_filters encode: {e}")))?;
    let mut tx = pool.begin().await.map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_type_id, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, $6, 'Perspective', $7, 'Wake', 'substrate',
                 'self-v1', $8, $9, 0)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(self_draft.schema_id.as_str())
    .bind(i32::try_from(self_draft.schema_version.into_inner()).unwrap_or(1))
    .bind(&self_draft.text)
    .bind(&req.personality_type_id)
    .bind(instance_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    let sidecar_sql = format!(
        "INSERT INTO {sidecar}
         SELECT * FROM jsonb_populate_record(
             NULL::{sidecar},
             ($1::jsonb || jsonb_build_object('memory_id', $2::uuid))
         )",
        sidecar = self_sidecar.as_str(),
    );
    sqlx::query(&sidecar_sql)
        .bind(&self_draft.typed_payload)
        .bind(memory_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

    let change_seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_type_id, entity_personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Perspective', $5, $6, $7, $8, $9, 0)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(self_draft.schema_id.as_str())
    .bind(i32::try_from(self_draft.schema_version.into_inner()).unwrap_or(1))
    .bind(&req.personality_type_id)
    .bind(instance_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_config
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_type_id, personality_instance_id,
             current_self_perspective_memory_id, wake_filters, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.personality_type_id)
    .bind(instance_id)
    .bind(memory_id)
    .bind(filters_json)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    // PostgreSQL has no built-in `max(uuid)` aggregate (verified PG 17),
    // so use ORDER BY ... LIMIT 1 — UUIDv7 is monotonic per source so
    // binary ordering matches insertion order.
    let max_seq: Option<uuid::Uuid> = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT seq FROM proxima_core.change_event
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
         ORDER BY seq DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_cursor
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_type_id, personality_instance_id, last_considered_seq)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.personality_type_id)
    .bind(instance_id)
    .bind(max_seq.unwrap_or(uuid::Uuid::nil()))
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(InstantiatePersonalityResponse {
        instance_id: PersonalityInstanceId::new(instance_id),
    })
}

pub async fn set_wake_config(
    pool: &PgPool,
    req: &SetWakeConfigRequest,
) -> Result<SetWakeConfigResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let filters_json = serde_json::to_value(&req.wake_filters)
        .map_err(|e| StorageError::Internal(format!("wake_filters encode: {e}")))?;
    let result = sqlx::query(
        "UPDATE proxima_core.personality_wake_config
         SET wake_filters = $1, status = 'active', updated_at = now()
         WHERE owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4
           AND personality_type_id = $5
           AND personality_instance_id = $6
           AND status <> 'tombstoned'",
    )
    .bind(filters_json)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.personality_type_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(SetWakeConfigResponse {
        status: "active".into(),
    })
}

pub async fn tombstone_personality(
    pool: &PgPool,
    req: &proxima_core::TombstonePersonalityRequest,
) -> Result<proxima_core::TombstonePersonalityResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let mut tx = pool.begin().await.map_err(map_err)?;

    let result = sqlx::query(
        "UPDATE proxima_core.personality_wake_config
         SET status = 'tombstoned',
             tombstoned_at = now(),
             updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_type_id = $4
           AND personality_instance_id = $5
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.personality_type_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    if result.rows_affected() == 1 {
        tx.commit().await.map_err(map_err)?;
        return Ok(proxima_core::TombstonePersonalityResponse {
            status: "tombstoned".into(),
            idempotent_replay: false,
        });
    }

    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT status
         FROM proxima_core.personality_wake_config
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_type_id = $4
           AND personality_instance_id = $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.personality_type_id)
    .bind(req.personality_instance_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;

    match existing {
        Some((status,)) if status == "tombstoned" => Ok(proxima_core::TombstonePersonalityResponse {
            status: "tombstoned".into(),
            idempotent_replay: true,
        }),
        Some(_) => unreachable!("UPDATE excluded only tombstoned rows; non-tombstoned must have hit"),
        None => Err(StorageError::NotFound),
    }
}

pub async fn list_active_wake_configs(pool: &PgPool) -> Result<Vec<WakeConfigRow>, StorageError> {
    let rows: Vec<(
        String,
        uuid::Uuid,
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        serde_json::Value,
        String,
        uuid::Uuid,
    )> = sqlx::query_as(
        "SELECT c.owner_principal_kind,
                c.owner_principal_id,
                c.owner_org_id,
                c.personality_type_id,
                c.personality_instance_id,
                c.current_self_perspective_memory_id,
                c.wake_filters,
                c.status,
                cur.last_considered_seq
         FROM proxima_core.personality_wake_config c
         JOIN proxima_core.personality_wake_cursor cur
           ON cur.owner_principal_kind = c.owner_principal_kind
          AND cur.owner_principal_id = c.owner_principal_id
          AND cur.owner_org_id = c.owner_org_id
          AND cur.personality_type_id = c.personality_type_id
          AND cur.personality_instance_id = c.personality_instance_id
         WHERE c.status = 'active'
         ORDER BY c.owner_principal_kind, c.owner_principal_id, c.personality_type_id, c.personality_instance_id",
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(
            |(
                owner_kind,
                owner_principal_id,
                owner_org_id,
                personality_type_id,
                personality_instance_id,
                self_memory_id,
                wake_filters_json,
                status,
                last_considered_seq,
            )| WakeConfigRow {
                owner: owner_from_parts(&owner_kind, owner_principal_id, owner_org_id),
                personality_type_id,
                personality_instance_id: PersonalityInstanceId::new(personality_instance_id),
                current_self_perspective_memory_id: MemoryId::new(self_memory_id),
                wake_filters_json,
                status,
                last_considered_seq,
            },
        )
        .collect())
}

pub async fn mark_wake_config_needs_repair(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_config
         SET status = 'needs_repair', updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_type_id = $4
           AND personality_instance_id = $5
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance.personality_type_id)
    .bind(instance.personality_instance_id.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn list_change_events_after(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid, Option<String>, Option<uuid::Uuid>, i16)> = sqlx::query_as(
        "SELECT seq, entity_personality_type_id, entity_personality_instance_id, wake_chain_depth
         FROM proxima_core.change_event
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND seq > $4
         ORDER BY seq ASC
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(after)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for (seq, type_id, instance_id, depth) in rows {
        if let Some(event) = hydrate_change_event(pool, seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_type_id: type_id,
                authoring_personality_instance_id: instance_id.map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            });
        }
    }
    Ok(out)
}

pub async fn advance_wake_cursor(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
    last_considered_seq: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_cursor
         SET last_considered_seq = GREATEST(last_considered_seq, $1), updated_at = now()
         WHERE owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4
           AND personality_type_id = $5
           AND personality_instance_id = $6",
    )
    .bind(last_considered_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance.personality_type_id)
    .bind(instance.personality_instance_id.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn try_begin_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
    change_event_seq: uuid::Uuid,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.personality_wake_invocations
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_type_id, personality_instance_id, change_event_seq,
             status, started_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'running', now())
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      personality_type_id, personality_instance_id, change_event_seq)
         DO NOTHING
         RETURNING change_event_seq",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance.personality_type_id)
    .bind(instance.personality_instance_id.into_inner())
    .bind(change_event_seq)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(inserted.is_some())
}

pub async fn finish_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
    change_event_seq: uuid::Uuid,
    status: WakeInvocationStatus,
    turn_count: u16,
    cost_usd: f64,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_invocations
         SET status = $1, finished_at = now(), turn_count = $2, cost_usd = $3
         WHERE owner_principal_kind = $4
           AND owner_principal_id = $5
           AND owner_org_id = $6
           AND personality_type_id = $7
           AND personality_instance_id = $8
           AND change_event_seq = $9",
    )
    .bind(status.as_str())
    .bind(i16::try_from(turn_count).unwrap_or(i16::MAX))
    .bind(cost_usd)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance.personality_type_id)
    .bind(instance.personality_instance_id.into_inner())
    .bind(change_event_seq)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn load_memory_batch_facts(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let batch_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT e.source_batch_id
         FROM proxima_core.memories m
         JOIN proxima_core.events e ON e.event_id = m.event_id
         WHERE m.memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some(batch_id) = batch_id else {
        return Ok(Vec::new());
    };
    load_batch_facts_by_id(pool, owner, batch_id, sidecars).await
}

async fn load_batch_facts_by_id(
    pool: &PgPool,
    owner: &Owner,
    batch_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let mut out = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT m.memory_id, e.schema_version, row_to_json(s.*) AS payload, m.wake_chain_depth
             FROM proxima_core.memories m
             JOIN proxima_core.events e ON m.event_id = e.event_id
             JOIN {sidecar} s ON s.memory_id = m.memory_id
             WHERE e.source_batch_id = $1
               AND m.owner_principal_kind = $2
               AND m.owner_principal_id = $3
               AND m.schema_id = $4",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(uuid::Uuid, i32, serde_json::Value, i16)> = sqlx::query_as(&sql)
            .bind(batch_id)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        out.extend(
            rows.into_iter()
                .map(|(memory_id, schema_version, payload_json, depth)| FactRow {
                    memory_id: MemoryId::new(memory_id),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
                    payload_json,
                    wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
                }),
        );
    }
    Ok(out)
}

pub async fn load_abstraction_heads(
    pool: &PgPool,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let mut rows_all = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT m.memory_id, m.schema_version, m.text, row_to_json(s.*) AS payload,
                    m.created_at, m.wake_chain_depth
             FROM proxima_core.memories m
             JOIN {sidecar} s ON s.memory_id = m.memory_id
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND m.kind = 'Abstraction'
               AND m.schema_id = $3
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
               )
             ORDER BY m.created_at DESC, m.memory_id DESC
             LIMIT $4",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(
            uuid::Uuid,
            i32,
            String,
            serde_json::Value,
            time::OffsetDateTime,
            i16,
        )> = sqlx::query_as(&sql)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, text, payload_json, created_at, depth) in rows {
            rows_all.push((
                created_at,
                memory_id,
                AbstractionRow {
                    memory_id: MemoryId::new(memory_id),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
                    text,
                    payload_json,
                    wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
                },
            ));
        }
    }
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(|(_, _, row)| row)
        .collect())
}

pub async fn load_memory_by_id(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let head: Option<(Option<String>, String, i32, Option<String>, i16)> = sqlx::query_as(
        "SELECT kind, schema_id, schema_version, text, wake_chain_depth
         FROM proxima_core.memories
         WHERE memory_id = $1
           AND owner_principal_kind = $2
           AND owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind, schema_id, schema_version, text, depth)) = head else {
        return Ok(None);
    };
    let kind_str = kind.unwrap_or_else(|| "Fact".to_string());
    let payload_json = if let Some(spec) =
        sidecars.iter().find(|s| s.schema_id.as_str() == schema_id)
    {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT row_to_json(s.*) AS payload FROM {sidecar} s WHERE s.memory_id = $1",
            sidecar = sidecar.as_str(),
        );
        let row: Option<(serde_json::Value,)> = sqlx::query_as(&sql)
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(map_err)?;
        row.map(|(p,)| p).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    Ok(Some(MemorySnapshot {
        memory_id,
        kind: kind_str,
        schema_id: SchemaId::new(schema_id),
        schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
        text,
        wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
        payload_json,
    }))
}

pub async fn lookup_prior_personality_head(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
    schema_id: &SchemaId,
) -> Result<Option<MemoryId>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT memory_id
         FROM proxima_core.memories
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND schema_id = $3
           AND personality_type_id = $4
           AND personality_instance_id = $5
           AND kind = 'Perspective'
           AND NOT EXISTS (
                SELECT 1 FROM proxima_core.memories newer
                WHERE newer.supersedes = memories.memory_id
           )
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(schema_id.as_str())
    .bind(&instance.personality_type_id)
    .bind(instance.personality_instance_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

#[allow(clippy::too_many_lines)]
pub async fn append_personality_memories(
    pool: &PgPool,
    req: &PersonalityWriteRequest<'_>,
) -> Result<PersonalityWriteOutcome, StorageError> {
    if req.memories.is_empty() {
        return Ok(PersonalityWriteOutcome {
            memory_ids: Vec::new(),
        });
    }
    let output_sidecar_table = PgIdent::table(req.sidecar_table)?;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let mut tx = pool.begin().await.map_err(map_err)?;
    let mut memory_ids = Vec::with_capacity(req.memories.len());

    for memory in req.memories {
        let memory_id = uuid::Uuid::now_v7();
        let prior_head = if memory.kind == proxima_core::PersonalityMemoryKind::Perspective {
            lookup_prior_personality_head(pool, &req.owner, &req.instance, &memory.schema_id)
                .await?
                .map(MemoryId::into_inner)
        } else {
            None
        };
        memory_ids.push(MemoryId::new(memory_id));
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
                 schema_id, schema_version, kind, text, operator_kind, model_id,
                 prompt_version, personality_type_id, personality_instance_id,
                 wake_chain_depth, supersedes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Wake', $9, $10, $11, $12, $13, $14)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(memory.kind.as_str())
        .bind(&memory.text)
        .bind(req.model_id)
        .bind(req.prompt_version)
        .bind(&req.instance.personality_type_id)
        .bind(req.instance.personality_instance_id.into_inner())
        .bind(i16::try_from(req.wake_chain_depth.into_inner()).unwrap_or(i16::MAX))
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        let sidecar_sql = format!(
            "INSERT INTO {sidecar}
             SELECT * FROM jsonb_populate_record(
                 NULL::{sidecar},
                 ($1::jsonb || jsonb_build_object('memory_id', $2::uuid))
             )",
            sidecar = output_sidecar_table.as_str(),
        );
        sqlx::query(&sidecar_sql)
            .bind(&memory.typed_payload)
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;

        let change_seq = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
                 entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
                 entity_personality_type_id, entity_personality_instance_id,
                 wake_chain_depth, supersedes_memory_id)
             VALUES ($1, $2, $3, $4, 'EntityAppend', $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.kind.as_str())
        .bind(memory_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(&req.instance.personality_type_id)
        .bind(req.instance.personality_instance_id.into_inner())
        .bind(i16::try_from(req.wake_chain_depth.into_inner()).unwrap_or(i16::MAX))
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        for prov_id in &memory.provenance {
            let target_kind = memory_kind_for_provenance(&mut tx, *prov_id).await?;
            let authorship_kind = provenance_edge_authorship_kind(memory.kind);
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.provenance_relation,
                source_kind: memory.kind.as_str(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind,
                target_memory_id: Some(prov_id.into_inner()),
                target_goal_id: None,
                authorship_kind,
                authorship_owner_memory_id: Some(memory_id),
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        if let Some(prior_head) = prior_head {
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.supersedes_relation,
                source_kind: memory.kind.as_str(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: memory.kind.as_str(),
                target_memory_id: Some(prior_head),
                target_goal_id: None,
                authorship_kind: "Engine",
                authorship_owner_memory_id: None,
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        let dim = i32::try_from(memory.embedding.len())
            .map_err(|_| StorageError::ConstraintViolation("embedding dim too large".into()))?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec, dim,
                 owner_principal_kind, owner_principal_id, owner_org_id)
             VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8)",
        )
        .bind(memory.kind.as_str())
        .bind(memory_id)
        .bind(&memory.embedding_model_id)
        .bind(&memory.embedding)
        .bind(dim)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    tx.commit().await.map_err(map_err)?;
    Ok(PersonalityWriteOutcome { memory_ids })
}

fn provenance_edge_authorship_kind(kind: proxima_core::PersonalityMemoryKind) -> &'static str {
    match kind {
        proxima_core::PersonalityMemoryKind::Abstraction => "OperatorFtoA",
        proxima_core::PersonalityMemoryKind::Perspective => "OperatorAtoP",
    }
}

async fn memory_kind_for_provenance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    memory_id: MemoryId,
) -> Result<&'static str, StorageError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
            .bind(memory_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    match row.and_then(|(kind,)| kind) {
        Some(kind) if kind == "Abstraction" => Ok("Abstraction"),
        Some(kind) if kind == "Perspective" => Ok("Perspective"),
        Some(other) => Err(StorageError::Internal(format!(
            "unsupported provenance memory kind: {other}"
        ))),
        None => Ok("Fact"),
    }
}

fn owner_from_parts(kind: &str, principal_id: uuid::Uuid, org_id: uuid::Uuid) -> Owner {
    Owner {
        principal: match kind {
            "User" => Principal::User(proxima_core::UserId::new(principal_id)),
            _ => Principal::Group(proxima_core::GroupId::new(principal_id)),
        },
        org_id: proxima_core::OrgId::new(org_id),
    }
}

#[allow(dead_code)]
fn external_personality() -> (&'static str, uuid::Uuid) {
    (
        EXTERNAL_PERSONALITY_TYPE_ID,
        EXTERNAL_PERSONALITY_INSTANCE_ID,
    )
}
