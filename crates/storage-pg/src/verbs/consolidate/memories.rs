use proxima_core::personality::{
    AbstractionRow, FactRow, MemorySnapshot, PersonalityInstanceId, PersonalityRef,
    PersonalityWriteOutcome, PersonalityWriteRequest, ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
    SidecarSpec, WakeChainDepth,
};
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, MemoryId, Owner, OwnerPrincipalKind, SchemaId, SchemaVersion,
    StorageError,
};
use sqlx::PgPool;

use super::rows::owner_columns;
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

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
         WHERE m.memory_id = $1
           AND m.tombstoned_at IS NULL",
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
               AND m.schema_id = $4
               AND m.tombstoned_at IS NULL",
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
               AND m.tombstoned_at IS NULL
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
                      AND newer.tombstoned_at IS NULL
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

pub async fn load_perspective_heads(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    root_perspective_memory_id: MemoryId,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<MemorySnapshot>, StorageError> {
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
               AND m.kind = 'Perspective'
               AND m.schema_id = $3
               AND m.personality_instance_id = $4
               AND m.memory_id <> $5
               AND m.schema_id <> $6
               AND m.schema_id !~ '-self-v[0-9]+$'
               AND m.tombstoned_at IS NULL
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
                      AND newer.tombstoned_at IS NULL
               )
             ORDER BY m.created_at DESC, m.memory_id DESC
             LIMIT $7",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(
            uuid::Uuid,
            i32,
            Option<String>,
            serde_json::Value,
            time::OffsetDateTime,
            i16,
        )> = sqlx::query_as(&sql)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(instance.into_inner())
            .bind(root_perspective_memory_id.into_inner())
            .bind(ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, text, payload_json, created_at, depth) in rows {
            rows_all.push((
                created_at,
                memory_id,
                MemorySnapshot {
                    memory_id: MemoryId::new(memory_id),
                    kind: "Perspective".to_string(),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
                    text,
                    wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
                    payload_json,
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
    reader_personality_instance_id: Option<PersonalityInstanceId>,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let head: Option<(
        Option<EntityKind>,
        String,
        i32,
        Option<String>,
        i16,
        Option<uuid::Uuid>,
    )> = sqlx::query_as(
        "SELECT kind, schema_id, schema_version, text, wake_chain_depth, personality_instance_id
         FROM proxima_core.memories
         WHERE memory_id = $1
           AND owner_principal_kind = $2
           AND owner_principal_id = $3
           AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind, schema_id, schema_version, text, depth, personality_instance_id)) = head else {
        return Ok(None);
    };
    if !memory_visible_to_reader(
        pool,
        owner_kind,
        owner_principal_id,
        memory_id,
        reader_personality_instance_id,
        kind,
        personality_instance_id,
    )
    .await?
    {
        return Ok(None);
    }
    let kind_str = kind.unwrap_or(EntityKind::Fact).as_str().to_string();
    let payload_json =
        if let Some(spec) = sidecars.iter().find(|s| s.schema_id.as_str() == schema_id) {
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
            row.map_or(serde_json::Value::Null, |(p,)| p)
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

async fn memory_visible_to_reader(
    pool: &PgPool,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    memory_id: MemoryId,
    reader_personality_instance_id: Option<PersonalityInstanceId>,
    kind: Option<EntityKind>,
    memory_personality_instance_id: Option<uuid::Uuid>,
) -> Result<bool, StorageError> {
    let Some(reader) = reader_personality_instance_id else {
        return Ok(true);
    };
    if kind.is_none() {
        return Ok(true);
    }
    let Some(readable_id) = memory_personality_instance_id else {
        return Ok(false);
    };
    let reader_id = reader.into_inner();
    if reader_id == readable_id {
        return Ok(true);
    }
    let allowed: Option<(i32,)> = sqlx::query_as(
        "SELECT 1
           FROM proxima_core.read_scope_matrix r
		          JOIN proxima_core.memories m
		            ON m.owner_principal_kind = r.owner_principal_kind
		           AND m.owner_principal_id = r.owner_principal_id
		           AND m.memory_id = $5
		           AND m.tombstoned_at IS NULL
	         WHERE r.owner_principal_kind = $1
            AND r.owner_principal_id = $2
            AND r.reader_personality_instance_id = $3
            AND r.readable_personality_instance_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(reader_id)
    .bind(readable_id)
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(allowed.is_some())
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
           AND personality_instance_id = $4
           AND kind = 'Perspective'
           AND tombstoned_at IS NULL
           AND NOT EXISTS (
                SELECT 1 FROM proxima_core.memories newer
                WHERE newer.supersedes = memories.memory_id
                  AND newer.tombstoned_at IS NULL
           )
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(schema_id.as_str())
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
                 prompt_version, personality_instance_id,
                 wake_chain_depth, supersedes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Wake',
                     $9, $10, $11, $12, $13)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(memory.kind.entity_kind())
        .bind(&memory.text)
        .bind(req.model_id)
        .bind(req.prompt_version)
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
                 entity_personality_instance_id,
                 wake_chain_depth, supersedes_memory_id)
             VALUES ($1, $2, $3, $4, 'EntityAppend',
                     $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.kind.entity_kind())
        .bind(memory_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(req.instance.personality_instance_id.into_inner())
        .bind(i16::try_from(req.wake_chain_depth.into_inner()).unwrap_or(i16::MAX))
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        for prov_id in &memory.provenance {
            let target_kind = memory_kind_for_provenance(&mut tx, *prov_id).await?;
            if !should_append_personality_provenance_edge(target_kind) {
                continue;
            }
            let authorship_kind = provenance_edge_authorship_kind(memory.kind);
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.provenance_relation,
                source_kind: memory.kind.entity_kind(),
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
                source_kind: memory.kind.entity_kind(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: memory.kind.entity_kind(),
                target_memory_id: Some(prior_head),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::Engine,
                authorship_owner_memory_id: None,
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        let authored = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: req.authored_relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(req.current_root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: memory.kind.entity_kind(),
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &req.owner,
        };
        append_edge_in_tx(&mut tx, &authored, None).await?;

        let dim = i32::try_from(memory.embedding.len())
            .map_err(|_| StorageError::ConstraintViolation("embedding dim too large".into()))?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec, dim,
                 owner_principal_kind, owner_principal_id, owner_org_id)
             VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8)",
        )
        .bind(memory.kind.entity_kind())
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

fn should_append_personality_provenance_edge(target_kind: EntityKind) -> bool {
    target_kind != EntityKind::Perspective
}

fn provenance_edge_authorship_kind(
    kind: proxima_core::PersonalityMemoryKind,
) -> proxima_core::EdgeAuthorshipKind {
    use proxima_core::EdgeAuthorshipKind;
    match kind {
        proxima_core::PersonalityMemoryKind::Abstraction => EdgeAuthorshipKind::OperatorFtoA,
        proxima_core::PersonalityMemoryKind::Perspective => EdgeAuthorshipKind::OperatorAtoP,
    }
}

async fn memory_kind_for_provenance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    memory_id: MemoryId,
) -> Result<EntityKind, StorageError> {
    let row: Option<(Option<EntityKind>,)> = sqlx::query_as(
        "SELECT kind FROM proxima_core.memories
             WHERE memory_id = $1
               AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(row.and_then(|(kind,)| kind).unwrap_or(EntityKind::Fact))
}
