use std::collections::HashMap;

use futures_util::future::try_join_all;
use proxima_core::read_models::{
    AbstractionRow, FactRow, MemorySchemaSpec, MemorySnapshot, resolve_memory_schema,
};
use proxima_core::storage::{MemoryGraphIdentity, MemoryGraphPayloadRow};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EntityKind, MemoryId, Owner, OwnerRef, OwnerRefKind, SchemaId, SidecarPayload, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

/// # Errors
///
/// Returns a storage error when the backend or schema resolver rejects a
/// requested row.
pub async fn load_memory_batch_facts(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    memory_id: MemoryId,
    schemas: &[MemorySchemaSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let _ = (pool, pg_sidecars, owner, memory_id, schemas);
    Ok(Vec::new())
}

/// # Errors
///
/// Returns a storage error when the backend or schema resolver rejects a
/// requested row.
pub async fn load_abstraction_heads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    schemas: &[MemorySchemaSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let mut rows_all = Vec::new();
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for spec in schemas {
        if spec.kind != EntityKind::Abstraction {
            continue;
        }
        let rows: Vec<(uuid::Uuid, Vec<String>, time::OffsetDateTime)> = sqlx::query_as(
            "SELECT m.t, m.sidecar_tables,
                    COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01')
               FROM proxima_core.memory_head h
               JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
              WHERE h.owner_id = $1
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
        for (memory_id, sidecar_tables, created_at) in rows {
            let id = MemoryId::new(memory_id);
            validate_and_queue_payload(&mut ids_by_key, pg_sidecars, spec, &sidecar_tables, id)?;
            rows_all.push((created_at, memory_id, id, spec.schema_id.clone()));
        }
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut out = Vec::new();
    for (_, _, memory_id, schema_id) in rows_all.into_iter().take(limit) {
        let payload = payloads.remove(&memory_id);
        let spec = resolve_memory_schema(schemas, EntityKind::Abstraction, &schema_id)?;
        if spec.sidecar_table.as_deref().is_some_and(|table| {
            payload.is_none() && !pg_sidecars.is_owner_pinned_memory_sidecar_table(table)
        }) {
            return Err(StorageError::ConstraintViolation(format!(
                "required sidecar payload missing for memory {memory_id:?}"
            )));
        }
        let text = payload
            .as_ref()
            .and_then(SidecarPayload::graph_body)
            .unwrap_or_default();
        out.push(AbstractionRow {
            memory_id,
            schema_id,
            schema_version: spec.schema_version,
            text,
            payload,
        });
    }
    Ok(out)
}

/// # Errors
///
/// Returns a storage error when the backend or schema resolver rejects a
/// requested row.
pub async fn load_memory_by_id(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    memory_id: MemoryId,
    schemas: &[MemorySchemaSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let raw: Option<(String, String, OwnerRefKind, uuid::Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT m.kind::text, m.schema_id,
                o.kind::text::proxima_core.owner_kind, m.owner_id, m.sidecar_tables
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind_text, schema_id, owner_kind, owner_id, sidecar_tables)) = raw else {
        return Ok(None);
    };
    let Some(kind) = parse_memory_kind(&kind_text) else {
        return Ok(None);
    };
    let rows = [MemoryHydrationRow {
        memory_id: memory_id.into_inner(),
        kind,
        schema_id,
        owner: owner_from_kind(owner_kind, owner_id),
        sidecar_tables,
    }];
    let mut snapshots = snapshots_from_rows(pool, pg_sidecars, &rows, schemas).await?;
    Ok(snapshots.pop())
}

/// # Errors
///
/// Returns a storage error when the backend or schema resolver rejects a
/// requested row.
pub async fn load_memories_by_ids(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[proxima_core::OwnerRef],
    memory_ids: &[MemoryId],
    schemas: &[MemorySchemaSpec],
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
    let raw: Vec<(
        uuid::Uuid,
        String,
        String,
        OwnerRefKind,
        uuid::Uuid,
        Vec<String>,
    )> = sqlx::query_as(
        "SELECT m.t, m.kind::text, m.schema_id,
                    o.kind::text::proxima_core.owner_kind, m.owner_id, m.sidecar_tables
               FROM proxima_core.memory m
               JOIN proxima_core.owners o ON o.owner_id = m.owner_id
              WHERE m.owner_id = ANY($1::uuid[])
                AND m.t = ANY($2::uuid[])
              ORDER BY m.t",
    )
    .bind(&owner_ids)
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let rows = raw
        .into_iter()
        .filter_map(
            |(t, kind, schema_id, owner_kind, owner_id, sidecar_tables)| {
                Some(MemoryHydrationRow {
                    memory_id: t,
                    kind: parse_memory_kind(&kind)?,
                    schema_id,
                    owner: owner_from_kind(owner_kind, owner_id),
                    sidecar_tables,
                })
            },
        )
        .collect::<Vec<_>>();
    snapshots_from_rows(pool, pg_sidecars, &rows, schemas).await
}

/// # Errors
///
/// Returns a storage error when an admitted identity disappears, changes, or
/// has an invalid sidecar declaration.
pub async fn load_memory_graph_payloads(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    identities: &[MemoryGraphIdentity],
    schemas: &[MemorySchemaSpec],
    include_body: bool,
) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
    if identities.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = identities
        .iter()
        .map(|i| i.memory_id.into_inner())
        .collect();
    let raw: Vec<(uuid::Uuid, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, m.schema_id, m.sidecar_tables
           FROM proxima_core.memory m
          WHERE m.t = ANY($1::uuid[])",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let by_id = raw
        .into_iter()
        .map(|(id, kind, schema_id, sidecar_tables)| {
            let kind = parse_memory_kind(&kind).ok_or_else(|| {
                StorageError::ConstraintViolation(format!("invalid memory kind for {id}"))
            })?;
            Ok((id, (kind, schema_id, sidecar_tables)))
        })
        .collect::<Result<HashMap<_, _>, StorageError>>()?;
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for identity in identities {
        let Some((kind, schema_id, sidecar_tables)) = by_id.get(&identity.memory_id.into_inner())
        else {
            return Err(StorageError::ConstraintViolation(format!(
                "admitted memory {:?} disappeared during graph hydration",
                identity.memory_id
            )));
        };
        if *kind != identity.kind || schema_id != identity.schema_id.as_str() {
            return Err(StorageError::ConstraintViolation(format!(
                "admitted memory {:?} changed identity during graph hydration",
                identity.memory_id
            )));
        }
        validate_and_queue_payload(
            &mut ids_by_key,
            pg_sidecars,
            resolve_memory_schema(schemas, *kind, &identity.schema_id)?,
            sidecar_tables,
            identity.memory_id,
        )?;
    }
    let payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    for identity in identities {
        let spec = resolve_memory_schema(schemas, identity.kind, &identity.schema_id)?;
        if spec.sidecar_table.as_deref().is_some_and(|table| {
            !payloads.contains_key(&identity.memory_id)
                && !pg_sidecars.is_owner_pinned_memory_sidecar_table(table)
        }) {
            return Err(StorageError::ConstraintViolation(format!(
                "required sidecar payload missing for memory {:?}",
                identity.memory_id
            )));
        }
    }
    Ok(identities
        .iter()
        .map(|identity| {
            let payload = payloads.get(&identity.memory_id);
            MemoryGraphPayloadRow {
                memory_id: identity.memory_id,
                tags: payload.map(SidecarPayload::graph_tags),
                body: include_body
                    .then(|| payload.and_then(SidecarPayload::graph_body))
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

fn parse_memory_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" | "Fact" => Some(EntityKind::Fact),
        "abstraction" | "Abstraction" => Some(EntityKind::Abstraction),
        "perspective" | "Perspective" => Some(EntityKind::Perspective),
        _ => None,
    }
}

fn owner_from_kind(kind: OwnerRefKind, owner_id: uuid::Uuid) -> OwnerRef {
    match kind {
        OwnerRefKind::Personal => OwnerRef::Personal(proxima_core::UserId::new(owner_id)),
        OwnerRefKind::Group => OwnerRef::Group(proxima_core::GroupId::new(owner_id)),
    }
}

#[derive(Debug)]
struct MemoryHydrationRow {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: String,
    owner: OwnerRef,
    sidecar_tables: Vec<String>,
}

async fn snapshots_from_rows(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    rows: &[MemoryHydrationRow],
    schemas: &[MemorySchemaSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for row in rows {
        let spec = resolve_memory_schema(schemas, row.kind, &SchemaId::new(row.schema_id.clone()))?;
        validate_and_queue_payload(
            &mut ids_by_key,
            pg_sidecars,
            spec,
            &row.sidecar_tables,
            MemoryId::new(row.memory_id),
        )?;
    }
    let mut payloads = load_memory_sidecar_payloads_batch(pool, pg_sidecars, ids_by_key).await?;
    let mut snapshots = Vec::with_capacity(rows.len());
    for row in rows {
        let id = MemoryId::new(row.memory_id);
        let spec = resolve_memory_schema(schemas, row.kind, &SchemaId::new(row.schema_id.clone()))?;
        let payload = payloads.remove(&id);
        if spec.sidecar_table.as_deref().is_some_and(|table| {
            payload.is_none() && !pg_sidecars.is_owner_pinned_memory_sidecar_table(table)
        }) {
            return Err(StorageError::ConstraintViolation(format!(
                "required sidecar payload missing for memory {id:?}"
            )));
        }
        snapshots.push(MemorySnapshot {
            memory_id: id,
            kind: row.kind,
            schema_id: spec.schema_id.clone(),
            schema_version: spec.schema_version,
            owner: row.owner,
            text: payload.as_ref().and_then(SidecarPayload::graph_body),
            payload,
        });
    }
    Ok(snapshots)
}

fn validate_and_queue_payload(
    ids_by_key: &mut HashMap<PgSidecarKey, Vec<MemoryId>>,
    pg_sidecars: &PgSidecarRegistryFrozen,
    spec: &MemorySchemaSpec,
    stamped_tables: &[String],
    memory_id: MemoryId,
) -> Result<(), StorageError> {
    let Some(table) = spec.sidecar_table.as_deref() else {
        return Ok(());
    };
    let payload_kind = payload_kind_for(spec.kind).expect("memory kind has payload kind");
    let key = PgSidecarKey::new(payload_kind, spec.schema_id.clone(), spec.schema_version);
    if pg_sidecars.table_for_schema(payload_kind, &spec.schema_id, spec.schema_version)
        != Some(table)
        || !stamped_tables.iter().any(|stamped| stamped == table)
    {
        return Err(StorageError::ConstraintViolation(format!(
            "memory {memory_id:?} has invalid sidecar stamp for {table}"
        )));
    }
    ids_by_key.entry(key).or_default().push(memory_id);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{MemoryHydrationRow, snapshots_from_rows};
    use crate::sidecars::core_pg_sidecars;
    use proxima_core::read_models::MemorySchemaSpec;
    use proxima_core::{
        AgentNoteV1, EntityKind, FactPayload, OwnerRef, SchemaVersion, UploadV1, UserId,
    };
    use sqlx::postgres::PgPoolOptions;

    fn fact_spec<P: FactPayload>() -> MemorySchemaSpec {
        MemorySchemaSpec {
            kind: EntityKind::Fact,
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            sidecar_table: P::sidecar_table().map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn one_invalid_visible_row_fails_the_whole_snapshot_batch() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1/unused")
            .expect("lazy pool needs no server");
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let rows = [
            MemoryHydrationRow {
                memory_id: uuid::Uuid::now_v7(),
                kind: EntityKind::Fact,
                schema_id: UploadV1::SCHEMA_ID.to_owned(),
                owner,
                sidecar_tables: Vec::new(),
            },
            MemoryHydrationRow {
                memory_id: uuid::Uuid::now_v7(),
                kind: EntityKind::Fact,
                schema_id: AgentNoteV1::SCHEMA_ID.to_owned(),
                owner,
                sidecar_tables: vec!["proxima_core.write_act_v1".to_owned()],
            },
        ];
        let schemas = [fact_spec::<UploadV1>(), fact_spec::<AgentNoteV1>()];

        let err = snapshots_from_rows(&pool, &core_pg_sidecars(), &rows, &schemas)
            .await
            .expect_err("a bad visible row must not become a partial batch");
        assert!(err.to_string().contains("invalid sidecar stamp"));
    }
}
