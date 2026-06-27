use std::collections::HashMap;

use futures_util::future::try_join_all;
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::personality::{
    AbstractionRow, FactRow, MemorySnapshot, PersonalityInstanceId, PersonalityRef,
    PersonalityWriteOutcome, PersonalityWriteRequest, SidecarSpec, WakeChainDepth,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, MemoryId, Owner, OwnerPrincipalKind, SchemaId, SchemaVersion,
    SidecarPayload, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::entity_owner::insert_entity_owner_home;

const PERSONALITY_APPEND_LOCK_KEY_DOMAIN: &[u8] = b"personality_append_lock_v1";

pub async fn load_memory_batch_facts(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
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
    load_batch_facts_by_id(pool, pg_sidecars, owner, batch_id, sidecars).await
}

async fn load_batch_facts_by_id(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    batch_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let sql = "SELECT m.memory_id, e.schema_version, m.wake_chain_depth
             FROM proxima_core.memories m
             JOIN proxima_core.events e ON m.event_id = e.event_id
             WHERE e.source_batch_id = $1
               AND m.owner_principal_kind = $2
               AND m.owner_principal_id = $3
               AND m.schema_id = $4
               AND e.schema_version = $5
               AND m.tombstoned_at IS NULL";
        let rows: Vec<(uuid::Uuid, i32, i16)> = sqlx::query_as(sql)
            .bind(batch_id)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(i32::try_from(spec.schema_version.into_inner()).unwrap_or(i32::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, depth) in rows {
            let memory_id = MemoryId::new(memory_id);
            let schema_version = SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1));
            queue_memory_sidecar_payload(
                &mut ids_by_key,
                pg_sidecars,
                PayloadKind::Fact,
                spec.schema_id.clone(),
                schema_version,
                memory_id,
            );
            rows_all.push((
                memory_id,
                spec.schema_id.clone(),
                schema_version,
                WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            ));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    Ok(rows_all
        .into_iter()
        .map(
            |(memory_id, schema_id, schema_version, wake_chain_depth)| FactRow {
                memory_id,
                schema_id,
                schema_version,
                payload: payloads.remove(&memory_id),
                wake_chain_depth,
            },
        )
        .collect())
}

pub async fn load_abstraction_heads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let sql = "SELECT m.memory_id, m.schema_version, m.text,
                    m.created_at, m.wake_chain_depth
             FROM proxima_core.memories m
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND m.kind = 'Abstraction'
               AND m.schema_id = $3
               AND m.schema_version = $4
               AND m.tombstoned_at IS NULL
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
                      AND newer.tombstoned_at IS NULL
               )
             ORDER BY m.created_at DESC, m.memory_id DESC
             LIMIT $5";
        let rows: Vec<(uuid::Uuid, i32, String, time::OffsetDateTime, i16)> = sqlx::query_as(sql)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(i32::try_from(spec.schema_version.into_inner()).unwrap_or(i32::MAX))
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, text, created_at, depth) in rows {
            let id = MemoryId::new(memory_id);
            let schema_version = SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1));
            queue_memory_sidecar_payload(
                &mut ids_by_key,
                pg_sidecars,
                PayloadKind::Abstraction,
                spec.schema_id.clone(),
                schema_version,
                id,
            );
            rows_all.push((
                created_at,
                memory_id,
                id,
                spec.schema_id.clone(),
                schema_version,
                text,
                WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            ));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(
            |(_, _, memory_id, schema_id, schema_version, text, wake_chain_depth)| AbstractionRow {
                memory_id,
                schema_id,
                schema_version,
                text,
                payload: payloads.remove(&memory_id),
                wake_chain_depth,
            },
        )
        .collect())
}

pub async fn load_perspective_heads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    instance: PersonalityInstanceId,
    root_perspective_memory_id: MemoryId,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let sql = "SELECT m.memory_id, m.schema_version, m.text,
                    m.created_at, m.wake_chain_depth
             FROM proxima_core.memories m
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND m.kind = 'Perspective'
               AND m.schema_id = $3
               AND m.schema_version = $4
               AND m.personality_instance_id = $5
               AND m.memory_id <> $6
               AND m.schema_id !~ '-self-v[0-9]+$'
               AND m.tombstoned_at IS NULL
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
                      AND newer.tombstoned_at IS NULL
               )
             ORDER BY m.created_at DESC, m.memory_id DESC
             LIMIT $7";
        let rows: Vec<(uuid::Uuid, i32, Option<String>, time::OffsetDateTime, i16)> =
            sqlx::query_as(sql)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(spec.schema_id.as_str())
                .bind(i32::try_from(spec.schema_version.into_inner()).unwrap_or(i32::MAX))
                .bind(instance.into_inner())
                .bind(root_perspective_memory_id.into_inner())
                .bind(i64::try_from(limit).unwrap_or(i64::MAX))
                .fetch_all(pool)
                .await
                .map_err(map_err)?;
        for (memory_id, schema_version, text, created_at, depth) in rows {
            let id = MemoryId::new(memory_id);
            let schema_version = SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1));
            queue_memory_sidecar_payload(
                &mut ids_by_key,
                pg_sidecars,
                PayloadKind::Perspective,
                spec.schema_id.clone(),
                schema_version,
                id,
            );
            rows_all.push((
                created_at,
                memory_id,
                id,
                spec.schema_id.clone(),
                schema_version,
                text,
                WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            ));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(
            |(_, _, memory_id, schema_id, schema_version, text, wake_chain_depth)| MemorySnapshot {
                memory_id,
                kind: "Perspective".to_string(),
                schema_id,
                schema_version,
                authoring_personality_instance_id: Some(instance),
                text,
                wake_chain_depth,
                payload: payloads.remove(&memory_id),
            },
        )
        .collect())
}

pub async fn load_memory_by_id(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    memory_id: MemoryId,
    reader_personality_instance_id: Option<PersonalityInstanceId>,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
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
    let payload = if let Some(spec) = sidecars.iter().find(|s| {
        s.schema_id.as_str() == schema_id
            && s.schema_version.into_inner() == u32::try_from(schema_version).unwrap_or(0)
    }) {
        let payload_kind = match kind.unwrap_or(EntityKind::Fact) {
            EntityKind::Fact => PayloadKind::Fact,
            EntityKind::Abstraction => PayloadKind::Abstraction,
            EntityKind::Perspective => PayloadKind::Perspective,
            EntityKind::Goal => return Ok(None),
        };
        load_memory_sidecar_payload(pool, pg_sidecars, payload_kind, spec, memory_id).await?
    } else {
        None
    };
    Ok(Some(MemorySnapshot {
        memory_id,
        kind: kind_str,
        schema_id: SchemaId::new(schema_id),
        schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
        authoring_personality_instance_id: decode_personality(personality_instance_id),
        text,
        wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
        payload,
    }))
}

async fn load_memory_sidecar_payload(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    kind: PayloadKind,
    spec: &SidecarSpec,
    memory_id: MemoryId,
) -> Result<Option<SidecarPayload>, StorageError> {
    let key = PgSidecarKey::new(kind, spec.schema_id.clone(), spec.schema_version);
    if !pg_sidecars.contains(&key) {
        return Ok(None);
    }
    let memory_ids = [memory_id];
    let mut payloads = pg_sidecars
        .load_memory_payloads_batch(pool, &key, &memory_ids)
        .await?;
    Ok(payloads.pop().map(|(_memory_id, payload)| payload))
}

fn queue_memory_sidecar_payload(
    ids_by_key: &mut HashMap<PgSidecarKey, Vec<MemoryId>>,
    pg_sidecars: &PgSidecarRegistryFrozen,
    kind: PayloadKind,
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    memory_id: MemoryId,
) {
    let key = PgSidecarKey::new(kind, schema_id, schema_version);
    if pg_sidecars.contains(&key) {
        ids_by_key.entry(key).or_default().push(memory_id);
    }
}

async fn load_memory_sidecar_payloads_batch(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    ids_by_key: HashMap<PgSidecarKey, Vec<MemoryId>>,
) -> Result<HashMap<MemoryId, SidecarPayload>, StorageError> {
    let batches = ids_by_key.into_iter().map(|(key, ids)| async move {
        pg_sidecars
            .load_memory_payloads_batch(pool, &key, &ids)
            .await
    });
    let rows = try_join_all(batches).await?;
    Ok(rows.into_iter().flatten().collect())
}

fn decode_personality(instance_id: Option<uuid::Uuid>) -> Option<PersonalityInstanceId> {
    instance_id
        .filter(|id| !id.is_nil())
        .map(PersonalityInstanceId::new)
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

pub async fn lookup_prior_personality_head<'e, E>(
    executor: E,
    owner: &Owner,
    instance: &PersonalityRef,
    schema_id: &SchemaId,
) -> Result<Option<MemoryId>, StorageError>
where
    E: sqlx::PgExecutor<'e>,
{
    let (owner_kind, owner_principal_id) = owner.columns();
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
    .fetch_optional(executor)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

#[allow(clippy::too_many_lines)]
pub async fn append_personality_memories(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &PersonalityWriteRequest<'_>,
) -> Result<PersonalityWriteOutcome, StorageError> {
    if req.memories.is_empty() {
        return Ok(PersonalityWriteOutcome {
            memory_ids: Vec::new(),
        });
    }
    let (owner_kind, owner_principal_id) = req.owner.columns();
    let mut tx = pool.begin().await.map_err(map_err)?;
    let mut memory_ids = Vec::with_capacity(req.memories.len());
    let has_perspective = req
        .memories
        .iter()
        .any(|memory| memory.kind == proxima_core::PersonalityMemoryKind::Perspective);
    if has_perspective {
        let key = lock_key(req.instance.personality_instance_id.into_inner());
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_err)?;
    }

    for memory in req.memories {
        let memory_id = uuid::Uuid::now_v7();
        // Read the head WITHIN the transaction so earlier inserts in this same
        // batch are visible. The transaction advisory lock serializes
        // cross-request Perspective appends for this personality instance.
        let prior_head = if memory.kind == proxima_core::PersonalityMemoryKind::Perspective {
            lookup_prior_personality_head(&mut *tx, &req.owner, &req.instance, &memory.schema_id)
                .await?
                .map(MemoryId::into_inner)
        } else {
            None
        };
        memory_ids.push(MemoryId::new(memory_id));
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_principal_kind, owner_principal_id,
                 schema_id, schema_version, kind, text, operator_kind, model_id,
                 prompt_version, personality_instance_id,
                 wake_chain_depth, supersedes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'Wake',
                     $8, $9, $10, $11, $12)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
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
        insert_entity_owner_home(
            &mut tx,
            memory_id,
            &req.owner,
            Some(req.instance.personality_instance_id.into_inner()),
        )
        .await?;

        sidecars
            .insert_memory_sidecar(&mut tx, MemoryId::new(memory_id), &memory.sidecar_payload)
            .await?;

        let change_seq = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, kind,
                 entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
                 entity_personality_instance_id,
                 wake_chain_depth, supersedes_memory_id)
             VALUES ($1, $2, $3, 'EntityAppend',
                     $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
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
                source_fact_entity_id: None,
                target_kind,
                target_memory_id: Some(prov_id.into_inner()),
                target_goal_id: None,
                target_fact_entity_id: None,
                authorship_kind,
                authorship_owner_memory_id: Some(memory_id),
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft).await?;
        }

        if let Some(prior_head) = prior_head {
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.supersedes_relation,
                source_kind: memory.kind.entity_kind(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                source_fact_entity_id: None,
                target_kind: memory.kind.entity_kind(),
                target_memory_id: Some(prior_head),
                target_goal_id: None,
                target_fact_entity_id: None,
                authorship_kind: EdgeAuthorshipKind::Engine,
                authorship_owner_memory_id: None,
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft).await?;
        }

        let authored = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: req.authored_relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(req.current_root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            source_fact_entity_id: None,
            target_kind: memory.kind.entity_kind(),
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &req.owner,
        };
        append_edge_in_tx(&mut tx, &authored).await?;

        if memory.embedding.len() != EMBEDDING_DIM {
            return Err(StorageError::ConstraintViolation(
                "embedding length must be 1024".into(),
            ));
        }
        let vec_literal = crate::pgvector::literal(&memory.embedding);
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec,
                 owner_principal_kind, owner_principal_id)
             VALUES ($1, $2, 1, $3, $4::vector, $5, $6)",
        )
        .bind(memory.kind.entity_kind())
        .bind(memory_id)
        .bind(&memory.embedding_model_id)
        .bind(vec_literal)
        .bind(owner_kind)
        .bind(owner_principal_id)
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

fn lock_key(instance_id: uuid::Uuid) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PERSONALITY_APPEND_LOCK_KEY_DOMAIN);
    hasher.update(instance_id.as_bytes());
    let hash = hasher.finalize();
    let bytes: [u8; 8] = hash.as_bytes()[..8]
        .try_into()
        .expect("blake3 hash is 32 bytes");
    i64::from_le_bytes(bytes)
}
