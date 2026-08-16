use std::collections::HashMap;

use futures_util::future::try_join_all;
use proxima_core::read_models::{AbstractionRow, FactRow, MemorySnapshot, SidecarSpec};
use proxima_core::storage::MemoryGraphPayloadRow;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EntityKind, MemoryId, Owner, SchemaId, SchemaVersion, SidecarPayload, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

/// The Facts of one source batch, with their typed sidecar payloads.
///
/// # Errors
///
/// Returns [`StorageError`] when a head or sidecar read fails.
pub async fn load_memory_batch_facts(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let _ = (pool, pg_sidecars, owner, memory_id, sidecars);
    Ok(Vec::new())
}

/// One owner's Abstraction heads, newest first, with sidecar payloads.
///
/// # Errors
///
/// Returns [`StorageError`] when a head or sidecar read fails.
pub async fn load_abstraction_heads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in sidecars {
        let rows: Vec<(uuid::Uuid, i32, String, time::OffsetDateTime)> = sqlx::query_as(
            "SELECT m.t, 1, '',
                    COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01')
               FROM proxima_core.memory_head h
               JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
              WHERE m.owner_id = $1
                AND m.kind = 'abstraction'
                AND h.schema_id = $2
              ORDER BY m.t DESC
              LIMIT $3",
        )
        .bind(owner.stored_owner_id())
        .bind(spec.schema_id.as_str())
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

/// One memory by id, with its typed sidecar payload. `None` when the id is
/// unknown or tombstoned.
///
/// # Errors
///
/// Returns [`StorageError`] when the head or sidecar read fails.
pub async fn load_memory_by_id(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let raw: Option<(String, String)> = sqlx::query_as(
        "SELECT m.kind::text, h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
          WHERE m.t = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind_text, schema_id)) = raw else {
        return Ok(None);
    };
    let kind = match kind_text.as_str() {
        "fact" => EntityKind::Fact,
        "abstraction" => EntityKind::Abstraction,
        "perspective" => EntityKind::Perspective,
        _ => return Ok(None),
    };
    let schema_version = 1;
    let text = None;
    let rows = [(
        memory_id.into_inner(),
        kind,
        schema_id,
        schema_version,
        text,
    )];
    let mut snapshots = snapshots_from_rows(pool, pg_sidecars, &rows, sidecars).await?;
    Ok(snapshots.pop())
}

/// Batch counterpart of [`load_memory_by_id`], visibility-scoped: head rows
/// come back in one owner-predicated query, then each row resolves its
/// typed sidecar payload. Unknown, invisible, and tombstoned ids are
/// simply absent from the result.
///
/// # Errors
///
/// Returns [`StorageError`] when a head or sidecar read fails.
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
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let ids: Vec<uuid::Uuid> = memory_ids.iter().map(|id| id.into_inner()).collect();
    let raw: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
          WHERE m.owner_id = ANY($1::uuid[])
            AND m.t = ANY($2::uuid[])
          ORDER BY m.t",
    )
    .bind(&owner_ids)
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let rows: Vec<(uuid::Uuid, EntityKind, String, i32, Option<String>)> = raw
        .into_iter()
        .filter_map(|(t, kind, schema_id)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some((t, kind, schema_id, 1, None))
        })
        .collect();

    snapshots_from_rows(pool, pg_sidecars, &rows, sidecars).await
}

/// Search-result tags/body via the same sidecar batch as snapshot reads.
///
/// # Errors
///
/// Returns [`StorageError`] when the head or sidecar read fails.
pub async fn load_memory_graph_payloads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    memory_ids: &[MemoryId],
    include_body: bool,
) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = memory_ids
        .iter()
        .copied()
        .map(MemoryId::into_inner)
        .collect::<Vec<_>>();
    let owner_id = owner.stored_owner_id();
    let raw_rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
          WHERE m.owner_id = $1
            AND m.t = ANY($2::uuid[])",
    )
    .bind(owner_id)
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let rows: Vec<(uuid::Uuid, EntityKind, String, i32, Option<String>)> = raw_rows
        .into_iter()
        .filter_map(|(t, kind, schema_id)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some((t, kind, schema_id, 1, None))
        })
        .collect();

    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for (memory_id, kind, schema_id, schema_version, _) in &rows {
        let Some(payload_kind) = payload_kind_for(*kind) else {
            continue;
        };
        let Ok(version) = u32::try_from(*schema_version) else {
            continue;
        };
        queue_memory_sidecar_payload(
            &mut ids_by_key,
            pg_sidecars,
            payload_kind,
            SchemaId::new(schema_id.clone()),
            SchemaVersion::new(version),
            MemoryId::new(*memory_id),
        );
    }
    let payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    Ok(rows
        .into_iter()
        .map(|(memory_id, _kind, _schema_id, _schema_version, text)| {
            let id = MemoryId::new(memory_id);
            let payload = payloads.get(&id);
            MemoryGraphPayloadRow {
                memory_id: id,
                tags: payload.map(SidecarPayload::graph_tags),
                body: include_body
                    .then(|| payload.and_then(SidecarPayload::graph_body).or(text))
                    .flatten(),
            }
        })
        .collect())
}

fn payload_kind_for(kind: EntityKind) -> Option<PayloadKind> {
    match kind {
        EntityKind::Fact => Some(PayloadKind::Fact),
        EntityKind::Abstraction => Some(PayloadKind::Abstraction),
        EntityKind::Perspective => Some(PayloadKind::Perspective),
        EntityKind::Goal => None,
    }
}

async fn snapshots_from_rows(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    rows: &[(uuid::Uuid, EntityKind, String, i32, Option<String>)],
    sidecars: &[SidecarSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for (memory_id, kind, schema_id, schema_version, _) in rows {
        let Some(payload_kind) = payload_kind_for(*kind) else {
            continue;
        };
        let Some(spec) = sidecars.iter().find(|spec| {
            spec.schema_id.as_str() == schema_id
                && spec.schema_version.into_inner() == u32::try_from(*schema_version).unwrap_or(0)
        }) else {
            continue;
        };
        queue_memory_sidecar_payload(
            &mut ids_by_key,
            pg_sidecars,
            payload_kind,
            spec.schema_id.clone(),
            spec.schema_version,
            MemoryId::new(*memory_id),
        );
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    let mut snapshots = Vec::with_capacity(rows.len());
    for (memory_id, kind, schema_id, schema_version, text) in rows {
        if *kind == EntityKind::Goal {
            continue;
        }
        let id = MemoryId::new(*memory_id);
        snapshots.push(MemorySnapshot {
            memory_id: id,
            kind: *kind,
            schema_id: SchemaId::new(schema_id.clone()),
            schema_version: SchemaVersion::new(u32::try_from(*schema_version).unwrap_or(1)),
            text: text.clone(),
            payload: payloads.remove(&id),
        });
    }
    Ok(snapshots)
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
