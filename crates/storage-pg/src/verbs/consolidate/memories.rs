use std::collections::HashMap;

use futures_util::future::try_join_all;
use proxima_core::read_models::{AbstractionRow, FactRow, MemorySnapshot, SidecarSpec};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EntityKind, MemoryId, Owner, OwnerRefKind, SchemaId, SchemaVersion, SidecarPayload,
    StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

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
         JOIN proxima_core.fact_receipts e ON e.receipt_id = m.receipt_id
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
    let (owner_kind, owner_id) = owner.columns();
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let sql =
            "SELECT m.memory_id, e.schema_version
             FROM proxima_core.memories m
             JOIN proxima_core.fact_receipts e ON m.receipt_id = e.receipt_id
             WHERE e.source_batch_id = $1
               AND EXISTS (
                    SELECT 1
                      FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     WHERE eo.entity_id = m.memory_id
                       AND eo.owner_kind = $2
                       AND eo.owner_id = $3
)
               AND m.schema_id = $4
               AND e.schema_version = $5
               AND m.tombstoned_at IS NULL"
        ;
        // SQL-POLICY: fixed-fragment — `sql` is the literal above; all five
        // parameters are bound.
        let rows: Vec<(uuid::Uuid, i32)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(batch_id)
            .bind(owner_kind)
            .bind(owner_id)
            .bind(spec.schema_id.as_str())
            .bind(i32::try_from(spec.schema_version.into_inner()).unwrap_or(i32::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version) in rows {
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
            rows_all.push((memory_id, spec.schema_id.clone(), schema_version));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    Ok(rows_all
        .into_iter()
        .map(|(memory_id, schema_id, schema_version)| FactRow {
            memory_id,
            schema_id,
            schema_version,
            payload: payloads.remove(&memory_id),
        })
        .collect())
}

pub async fn load_abstraction_heads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let sql =
            "SELECT m.memory_id, m.schema_version, m.text,
                    m.created_at
             FROM proxima_core.memories m
             WHERE EXISTS (
                    SELECT 1
                      FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     WHERE eo.entity_id = m.memory_id
                       AND eo.owner_kind = $1
                       AND eo.owner_id = $2
)
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
             LIMIT $5"
        ;
        // SQL-POLICY: fixed-fragment — `sql` is the literal above; all five
        // parameters are bound.
        let rows: Vec<(uuid::Uuid, i32, String, time::OffsetDateTime)> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql))
                .bind(owner_kind)
                .bind(owner_id)
                .bind(spec.schema_id.as_str())
                .bind(i32::try_from(spec.schema_version.into_inner()).unwrap_or(i32::MAX))
                .bind(i64::try_from(limit).unwrap_or(i64::MAX))
                .fetch_all(pool)
                .await
                .map_err(map_err)?;
        for (memory_id, schema_version, text, created_at) in rows {
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
            ));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(
            |(_, _, memory_id, schema_id, schema_version, text)| AbstractionRow {
                memory_id,
                schema_id,
                schema_version,
                text,
                payload: payloads.remove(&memory_id),
            },
        )
        .collect())
}

pub async fn load_memory_by_id(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let head: Option<(
        Option<EntityKind>,
        String,
        i32,
        Option<String>,
        OwnerRefKind,
        Option<uuid::Uuid>,
    )> = sqlx::query_as(
        "SELECT m.kind, m.schema_id, m.schema_version, m.text,
                home_owner.owner_kind, home_owner.owner_id
         FROM proxima_core.memories m
         LEFT JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) home_owner
           ON home_owner.entity_id = m.memory_id
WHERE m.memory_id = $1
           AND m.tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind, schema_id, schema_version, text, _owner_kind, _owner_id)) = head else {
        return Ok(None);
    };
    snapshot_with_payload(
        pool,
        pg_sidecars,
        memory_id,
        kind,
        schema_id,
        schema_version,
        text,
        sidecars,
    )
    .await
}

/// Resolve a head row into a [`MemorySnapshot`], loading its typed sidecar
/// payload when the schema has one registered. Goal rows project to `None`
/// (goals are not memories-readable through this path).
#[allow(clippy::too_many_arguments)]
async fn snapshot_with_payload(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    memory_id: MemoryId,
    kind: Option<EntityKind>,
    schema_id: String,
    schema_version: i32,
    text: Option<String>,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
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
        text,
        payload,
    }))
}

/// Batch counterpart of [`load_memory_by_id`], visibility-scoped: head rows
/// come back in one owner-predicated query, then each row resolves its
/// typed sidecar payload. Unknown, invisible, and tombstoned ids are
/// simply absent from the result.
pub async fn load_memories_by_ids(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[proxima_core::OwnerRef],
    memory_ids: &[MemoryId],
    sidecars: &[SidecarSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    if read_owners.is_empty() || memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = crate::verbs::query::read_owner_columns(read_owners);
    let ids: Vec<uuid::Uuid> = memory_ids.iter().map(|id| id.into_inner()).collect();
    let sql = format!(
        "SELECT m.memory_id, m.kind, m.schema_id, m.schema_version, m.text
           FROM proxima_core.memories m
          WHERE EXISTS (
                    SELECT 1
                      FROM {entity_owner_union} eo
                      JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                        ON {read_owner_predicate}
                     WHERE eo.entity_id = m.memory_id
                )
            AND m.memory_id = ANY($3::uuid[])
            AND m.tombstoned_at IS NULL
          ORDER BY m.created_at, m.memory_id",
        entity_owner_union = crate::verbs::query::entity_owner_union(),
        read_owner_predicate = crate::verbs::query::read_owner_predicate("eo", "rs"),
    );
    // SQL-POLICY: fixed-fragment
    let rows: Vec<(uuid::Uuid, Option<EntityKind>, String, i32, Option<String>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(&read_owner_kinds)
            .bind(&read_owner_ids)
            .bind(&ids)
            .fetch_all(pool)
            .await
            .map_err(map_err)?;

    let mut snapshots = Vec::with_capacity(rows.len());
    for (memory_id, kind, schema_id, schema_version, text) in rows {
        if let Some(snapshot) = snapshot_with_payload(
            pool,
            pg_sidecars,
            MemoryId::new(memory_id),
            kind,
            schema_id,
            schema_version,
            text,
            sidecars,
        )
        .await?
        {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
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
        .load_memory_payloads_batch(PgSidecarReadCtx::from(pool), &key, &memory_ids)
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
            .load_memory_payloads_batch(PgSidecarReadCtx::from(pool), &key, &ids)
            .await
    });
    let rows = try_join_all(batches).await?;
    Ok(rows.into_iter().flatten().collect())
}
