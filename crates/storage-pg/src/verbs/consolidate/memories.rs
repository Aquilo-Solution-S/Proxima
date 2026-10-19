use std::collections::HashMap;

use proxima_core::read_models::{MemorySchemaSpec, MemorySnapshot, resolve_memory_schema};
use proxima_core::storage::{MemoryGraphIdentity, MemoryGraphPayloadRow};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{EntityKind, MemoryId, OwnerId, SchemaId, SidecarPayload, StorageError};
use sqlx::PgConnection;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub(crate) async fn load_memory_by_id_on_connection(
    connection: &mut PgConnection,
    pg_sidecars: &PgSidecarRegistryFrozen,
    memory_id: MemoryId,
    schemas: &[MemorySchemaSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let raw: Option<StoredMemoryRow> = sqlx::query_as(
        "SELECT m.t, m.kind::text AS kind, m.schema_id, m.owner_id, m.sidecar_tables, m.occurred_at, m.\"by\", m.via FROM proxima_core.memory m JOIN proxima_core.owners o ON o.owner_id = m.owner_id WHERE m.t = $1",
    ).bind(memory_id.into_inner()).fetch_optional(&mut *connection).await.map_err(map_err)?;
    let Some(row) = raw.and_then(StoredMemoryRow::into_hydration) else {
        return Ok(None);
    };
    let rows = [row];
    let mut snapshots =
        snapshots_from_rows_on_connection(connection, pg_sidecars, &rows, schemas).await?;
    Ok(snapshots.pop())
}

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub(crate) async fn load_memories_by_ids_on_connection(
    connection: &mut PgConnection,
    pg_sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[proxima_core::OwnerId],
    memory_ids: &[MemoryId],
    schemas: &[MemorySchemaSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    if read_owners.is_empty() || memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerId::into_inner)
        .collect();
    let ids: Vec<uuid::Uuid> = memory_ids.iter().map(|id| id.into_inner()).collect();
    let raw: Vec<StoredMemoryRow> = sqlx::query_as(
        "SELECT m.t, m.kind::text AS kind, m.schema_id, m.owner_id, m.sidecar_tables, m.occurred_at, m.\"by\", m.via FROM proxima_core.memory m JOIN proxima_core.owners o ON o.owner_id = m.owner_id WHERE m.owner_id = ANY($1::uuid[]) AND m.t = ANY($2::uuid[]) ORDER BY m.t",
    ).bind(&owner_ids).bind(&ids).fetch_all(&mut *connection).await.map_err(map_err)?;
    let rows = raw
        .into_iter()
        .filter_map(StoredMemoryRow::into_hydration)
        .collect::<Vec<_>>();
    snapshots_from_rows_on_connection(connection, pg_sidecars, &rows, schemas).await
}

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub(crate) async fn load_memory_graph_payloads_on_connection(
    connection: &mut PgConnection,
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
    let raw: Vec<(uuid::Uuid, String, String, Vec<String>)> = sqlx::query_as("SELECT m.t, m.kind::text, m.schema_id, m.sidecar_tables FROM proxima_core.memory m WHERE m.t = ANY($1::uuid[])").bind(&ids).fetch_all(&mut *connection).await.map_err(map_err)?;
    let by_id = raw
        .into_iter()
        .map(|(id, kind, schema_id, tables)| {
            let kind = parse_memory_kind(&kind).ok_or_else(|| {
                StorageError::ConstraintViolation(format!("invalid memory kind for {id}"))
            })?;
            Ok((id, (kind, schema_id, tables)))
        })
        .collect::<Result<HashMap<_, _>, StorageError>>()?;
    let mut ids_by_key = HashMap::new();
    for identity in identities {
        let Some((kind, schema_id, tables)) = by_id.get(&identity.memory_id.into_inner()) else {
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
            tables,
            identity.memory_id,
        )?;
    }
    let payloads =
        load_memory_sidecar_payloads_batch_on_connection(connection, pg_sidecars, ids_by_key)
            .await?;
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
    // Body and tags follow the schema's display rule, the same one by-id
    // reads use, so a search hit and a read of the same memory agree.
    let sketches = if include_body {
        let needs_fallback = identities
            .iter()
            .map(|identity| {
                let spec = resolve_memory_schema(schemas, identity.kind, &identity.schema_id)?;
                Ok(spec
                    .display_body(payloads.get(&identity.memory_id), None)
                    .is_none()
                    .then(|| identity.memory_id.into_inner()))
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        let needs_fallback: Vec<uuid::Uuid> = needs_fallback.into_iter().flatten().collect();
        crate::verbs::sketch::load_sketch_texts_on_connection(connection, &needs_fallback).await?
    } else {
        HashMap::new()
    };
    identities
        .iter()
        .map(|identity| {
            let spec = resolve_memory_schema(schemas, identity.kind, &identity.schema_id)?;
            let payload = payloads.get(&identity.memory_id);
            let sketch = sketches.get(&identity.memory_id.into_inner());
            Ok(MemoryGraphPayloadRow {
                memory_id: identity.memory_id,
                tags: payload.map(|payload| spec.display_tags(Some(payload))),
                body: include_body
                    .then(|| spec.display_body(payload, sketch.map(String::as_str)))
                    .flatten(),
            })
        })
        .collect()
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

/// One `memory` row as the by-id loaders select it, before its kind is
/// parsed.
#[derive(sqlx::FromRow)]
struct StoredMemoryRow {
    t: uuid::Uuid,
    kind: String,
    schema_id: String,
    owner_id: uuid::Uuid,
    by: uuid::Uuid,
    via: Option<uuid::Uuid>,
    sidecar_tables: Vec<String>,
    occurred_at: Option<time::OffsetDateTime>,
}

impl StoredMemoryRow {
    /// The row to hydrate, or `None` for a kind the memory reads do not
    /// serve.
    fn into_hydration(self) -> Option<MemoryHydrationRow> {
        Some(MemoryHydrationRow {
            memory_id: self.t,
            kind: parse_memory_kind(&self.kind)?,
            schema_id: self.schema_id,
            owner: OwnerId::new(self.owner_id),
            writer: proxima_core::WriterAttribution {
                by: self.by,
                via: self.via,
            },
            sidecar_tables: self.sidecar_tables,
            occurred_at: self.occurred_at,
        })
    }
}

#[derive(Debug)]
struct MemoryHydrationRow {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: String,
    owner: OwnerId,
    writer: proxima_core::WriterAttribution,
    sidecar_tables: Vec<String>,
    occurred_at: Option<time::OffsetDateTime>,
}

async fn snapshots_from_rows_on_connection(
    connection: &mut PgConnection,
    pg_sidecars: &PgSidecarRegistryFrozen,
    rows: &[MemoryHydrationRow],
    schemas: &[MemorySchemaSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let ids_by_key = snapshot_payload_keys(pg_sidecars, rows, schemas)?;
    let mut payloads =
        load_memory_sidecar_payloads_batch_on_connection(connection, pg_sidecars, ids_by_key)
            .await?;
    let needs_fallback = rows
        .iter()
        .map(|row| {
            let spec =
                resolve_memory_schema(schemas, row.kind, &SchemaId::new(row.schema_id.clone()))?;
            let id = MemoryId::new(row.memory_id);
            Ok(spec
                .display_body(payloads.get(&id), None)
                .is_none()
                .then_some(row.memory_id))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let needs_fallback: Vec<uuid::Uuid> = needs_fallback.into_iter().flatten().collect();
    let sketches =
        crate::verbs::sketch::load_sketch_texts_on_connection(connection, &needs_fallback).await?;
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
            writer: row.writer,
            occurred_at: row.occurred_at,
            text: spec.display_body(
                payload.as_ref(),
                sketches.get(&row.memory_id).map(String::as_str),
            ),
            tags: spec.display_tags(payload.as_ref()),
            payload,
        });
    }
    Ok(snapshots)
}

fn snapshot_payload_keys(
    pg_sidecars: &PgSidecarRegistryFrozen,
    rows: &[MemoryHydrationRow],
    schemas: &[MemorySchemaSpec],
) -> Result<HashMap<PgSidecarKey, Vec<MemoryId>>, StorageError> {
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
    Ok(ids_by_key)
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

async fn load_memory_sidecar_payloads_batch_on_connection(
    connection: &mut PgConnection,
    pg_sidecars: &PgSidecarRegistryFrozen,
    ids_by_key: HashMap<PgSidecarKey, Vec<MemoryId>>,
) -> Result<HashMap<MemoryId, SidecarPayload>, StorageError> {
    let mut result = HashMap::new();
    for (key, ids) in ids_by_key {
        let rows = pg_sidecars
            .load_memory_payloads_batch(PgSidecarReadCtx::from(&mut *connection), &key, &ids)
            .await?;
        result.extend(rows);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{MemoryHydrationRow, snapshot_payload_keys};
    use crate::sidecars::core_pg_sidecars;
    use proxima_core::read_models::MemorySchemaSpec;
    use proxima_core::{
        AgentNoteV1, EntityKind, FactPayload, OwnerId, SchemaVersion, UploadV1, UserId,
    };

    fn fact_spec<P: FactPayload>() -> MemorySchemaSpec {
        MemorySchemaSpec::new(
            EntityKind::Fact,
            P::schema_id(),
            SchemaVersion::new(P::SCHEMA_VERSION),
            P::sidecar_table().map(str::to_owned),
        )
    }

    #[test]
    fn one_invalid_visible_row_fails_the_whole_snapshot_batch() {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        let rows = [
            MemoryHydrationRow {
                writer: crate::test_fixtures::writer(),
                memory_id: uuid::Uuid::now_v7(),
                kind: EntityKind::Fact,
                schema_id: UploadV1::SCHEMA_ID.to_owned(),
                owner,
                sidecar_tables: Vec::new(),
                occurred_at: None,
            },
            MemoryHydrationRow {
                writer: crate::test_fixtures::writer(),
                memory_id: uuid::Uuid::now_v7(),
                kind: EntityKind::Fact,
                schema_id: AgentNoteV1::SCHEMA_ID.to_owned(),
                owner,
                sidecar_tables: vec!["proxima_core.write_act_v1".to_owned()],
                occurred_at: None,
            },
        ];
        let schemas = [fact_spec::<UploadV1>(), fact_spec::<AgentNoteV1>()];

        let err = snapshot_payload_keys(&core_pg_sidecars(), &rows, &schemas)
            .expect_err("a bad visible row must not become a partial batch");
        assert!(err.to_string().contains("invalid sidecar stamp"));
    }
}
