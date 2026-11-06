use std::collections::HashMap;

use proxiima_core::verbs::schema::PayloadKind;
use proxiima_core::{EntityKind, GoalId, MemoryId, OwnerId, StorageError};
use sqlx::{Postgres, Row, Transaction};

use super::rows::{MigrationMemoryRow, parse_memory_kind};
use super::{
    MigrationBlob, MigrationGoal, MigrationMemory, PgSchemaEvolution, PgSidecarKey,
    PgSidecarReadCtx,
};

/// Read the complete owner-scoped historical source set. The source schemas
/// come only from the frozen declaration; forgotten rows are intentionally
/// included because they remain migration inputs.
pub(crate) async fn read_sources(
    tx: &mut Transaction<'_, Postgres>,
    program: &PgSchemaEvolution,
    owner: OwnerId,
) -> Result<(Vec<MigrationMemory>, Vec<MigrationGoal>, Vec<MigrationBlob>), StorageError> {
    let schema_ids: Vec<_> = program
        .declaration
        .sources
        .iter()
        .map(|s| s.id.render())
        .collect();
    let kinds: Vec<_> = program
        .declaration
        .sources
        .iter()
        .map(|s| super::kind_name(s.kind))
        .collect();
    let mut memories = read_memories(tx, owner, &schema_ids, &kinds).await?;
    load_memory_payloads(tx, program, &mut memories).await?;
    let goal_schemas: Vec<_> = program
        .declaration
        .sources
        .iter()
        .filter(|s| s.kind == PayloadKind::Goal)
        .map(|s| s.id.render())
        .collect();
    let mut goals = read_goals(tx, owner, &goal_schemas).await?;
    load_goal_payloads(tx, program, &mut goals).await?;
    let blob_schemas: Vec<_> = program
        .declaration
        .sources
        .iter()
        .filter(|s| s.kind == PayloadKind::CitedObject)
        .map(|s| s.id.render())
        .collect();
    let blobs = read_blobs(tx, owner, &blob_schemas).await?;
    Ok((memories, goals, blobs))
}

fn source_version(program: &PgSchemaEvolution, schema_id: &str) -> Result<u32, StorageError> {
    program
        .declaration
        .sources
        .iter()
        .find(|source| source.schema_id().as_str() == schema_id)
        .map(|source| source.id.version)
        .ok_or_else(|| {
            StorageError::ConstraintViolation(format!("source schema {schema_id} is not declared"))
        })
}

fn memory_kind(kind: EntityKind) -> Option<PayloadKind> {
    match kind {
        EntityKind::Fact => Some(PayloadKind::Fact),
        EntityKind::Abstraction => Some(PayloadKind::Abstraction),
        EntityKind::Perspective => Some(PayloadKind::Perspective),
        EntityKind::Goal => None,
    }
}

async fn load_memory_payloads(
    tx: &mut Transaction<'_, Postgres>,
    program: &PgSchemaEvolution,
    memories: &mut [MigrationMemory],
) -> Result<(), StorageError> {
    let mut groups: HashMap<PgSidecarKey, Vec<MemoryId>> = HashMap::new();
    for memory in memories.iter() {
        if memory.row.sidecar_tables.is_empty() {
            continue;
        }
        let kind = memory_kind(memory.kind()).ok_or_else(|| {
            StorageError::ConstraintViolation("Goal cannot be a Memory sidecar".into())
        })?;
        let version = source_version(program, &memory.row.schema_id)?;
        let key = PgSidecarKey::new(
            kind,
            proxiima_core::SchemaId::new(memory.row.schema_id.clone()),
            proxiima_core::SchemaVersion::new(version),
        );
        groups.entry(key).or_default().push(memory.id());
    }
    let mut loaded = HashMap::new();
    for (key, ids) in groups {
        let entry = program.schemas.registry.entries.get(&key).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "no migration sidecar loader for {}",
                key.schema_id.as_str()
            ))
        })?;
        let batch_loader = entry
            .memory_load_batch
            .ok_or_else(|| super::refused("migration source has no typed memory loader"))?;
        for memory in memories.iter().filter(|m| ids.contains(&m.id())) {
            if memory
                .row
                .sidecar_tables
                .iter()
                .any(|table| table != &entry.sidecar_table)
            {
                return Err(super::refused(
                    "source carries additional sidecars without a complete typed migration",
                ));
            }
        }
        let ctx = PgSidecarReadCtx::from(&mut **tx).for_registered_table(&entry.sidecar_table);
        for (id, payload) in batch_loader(ctx, key.kind, &ids).await? {
            if !ids.contains(&id)
                || payload.kind != key.kind
                || payload.schema_id != key.schema_id
                || payload.schema_version != key.schema_version
            {
                return Err(super::refused(
                    "migration loader returned a foreign identity or schema",
                ));
            }
            if loaded.insert(id, payload).is_some() {
                return Err(StorageError::ConstraintViolation(format!(
                    "duplicate migration sidecar payload for {id:?}"
                )));
            }
        }
        for id in ids {
            if !loaded.contains_key(&id) {
                return Err(StorageError::ConstraintViolation(format!(
                    "missing migration sidecar payload for {id:?}"
                )));
            }
        }
    }
    for memory in memories {
        if let Some(payload) = loaded.remove(&memory.id()) {
            memory.payload = Some(payload);
        }
    }
    Ok(())
}

async fn load_goal_payloads(
    tx: &mut Transaction<'_, Postgres>,
    program: &PgSchemaEvolution,
    goals: &mut [MigrationGoal],
) -> Result<(), StorageError> {
    let mut groups: HashMap<PgSidecarKey, Vec<GoalId>> = HashMap::new();
    for goal in goals.iter() {
        let version = source_version(program, goal.schema_id())?;
        let key = PgSidecarKey::new(
            PayloadKind::Goal,
            proxiima_core::SchemaId::new(goal.schema_id().to_owned()),
            proxiima_core::SchemaVersion::new(version),
        );
        if program.schemas.goal_loaders.contains_key(&key) {
            groups.entry(key).or_default().push(goal.id());
        }
    }
    for (key, ids) in groups {
        let goal_loader = program
            .schemas
            .goal_loaders
            .get(&key)
            .expect("group key was checked");
        let mut goal_payloads = HashMap::new();
        let entry = program
            .schemas
            .registry
            .entries
            .get(&key)
            .ok_or_else(|| super::refused("Goal source lacks its registered sidecar"))?;
        let ctx = PgSidecarReadCtx::from(&mut **tx).for_registered_table(&entry.sidecar_table);
        for (id, payload) in goal_loader(ctx, &ids).await? {
            if !ids.contains(&id)
                || payload.kind != key.kind
                || payload.schema_id != key.schema_id
                || payload.schema_version != key.schema_version
            {
                return Err(super::refused(
                    "migration Goal loader returned a foreign identity or schema",
                ));
            }
            if goal_payloads.insert(id, payload).is_some() {
                return Err(StorageError::ConstraintViolation(format!(
                    "duplicate Goal sidecar payload for {id:?}"
                )));
            }
        }
        for goal in goals.iter_mut().filter(|goal| ids.contains(&goal.id())) {
            goal.sidecar = Some(goal_payloads.remove(&goal.id()).ok_or_else(|| {
                StorageError::ConstraintViolation(format!(
                    "missing Goal sidecar payload for {:?}",
                    goal.id()
                ))
            })?);
        }
    }
    Ok(())
}

async fn read_memories(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    schema_ids: &[String],
    kinds: &[&str],
) -> Result<Vec<MigrationMemory>, StorageError> {
    let rows = sqlx::query(
        "SELECT handle,t,kind::text,owner_id,schema_id,source_id,ingest_key,blob_id,content_id,
                origins,refs,goal_refs,guided_by,forked_from,sidecar_tables,occurred_at,\"by\",via,
                lexical_language::text,forgotten_at
           FROM proxiima_core.memory
          /* FORGET-POLICY: includes-forgotten (all historical representations migrate) */
          WHERE owner_id = $1 AND (schema_id,kind::text) IN (SELECT * FROM unnest($2::text[],$3::text[]))
          ORDER BY t",
    )
    .bind(owner.into_inner())
    .bind(schema_ids)
    .bind(kinds)
    .fetch_all(&mut **tx)
    .await
    .map_err(crate::error::map_err)?;
    rows.into_iter()
        .map(|row| {
            let kind = parse_memory_kind(
                &row.try_get::<String, _>("kind")
                    .map_err(crate::error::map_err)?,
            )
            .map_err(StorageError::ConstraintViolation)?;
            Ok(MigrationMemory {
                row: MigrationMemoryRow {
                    handle: row.try_get("handle").map_err(crate::error::map_err)?,
                    t: MemoryId::new(row.try_get("t").map_err(crate::error::map_err)?),
                    kind,
                    owner_id: OwnerId::new(row.try_get("owner_id").map_err(crate::error::map_err)?),
                    schema_id: row.try_get("schema_id").map_err(crate::error::map_err)?,
                    source_id: row.try_get("source_id").map_err(crate::error::map_err)?,
                    ingest_key: row.try_get("ingest_key").map_err(crate::error::map_err)?,
                    blob_id: row.try_get("blob_id").map_err(crate::error::map_err)?,
                    content_id: row.try_get("content_id").map_err(crate::error::map_err)?,
                    origins: row.try_get("origins").map_err(crate::error::map_err)?,
                    refs: row.try_get("refs").map_err(crate::error::map_err)?,
                    goal_refs: row.try_get("goal_refs").map_err(crate::error::map_err)?,
                    guided_by: row.try_get("guided_by").map_err(crate::error::map_err)?,
                    forked_from: row.try_get("forked_from").map_err(crate::error::map_err)?,
                    sidecar_tables: row
                        .try_get("sidecar_tables")
                        .map_err(crate::error::map_err)?,
                    occurred_at: row.try_get("occurred_at").map_err(crate::error::map_err)?,
                    by: row.try_get("by").map_err(crate::error::map_err)?,
                    via: row.try_get("via").map_err(crate::error::map_err)?,
                    lexical_language: row
                        .try_get("lexical_language")
                        .map_err(crate::error::map_err)?,
                    forgotten_at: row.try_get("forgotten_at").map_err(crate::error::map_err)?,
                },
                payload: None,
            })
        })
        .collect()
}

async fn read_goals(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    schema_ids: &[String],
) -> Result<Vec<MigrationGoal>, StorageError> {
    let rows = sqlx::query(
        "SELECT g.handle,g.t,g.owner_id,g.schema_id,g.\"by\",g.title,g.text,g.state,
                g.request_id,g.close_fact_t,g.assignment_t,g.dependency_t,g.evidence_t,
                g.motivated_by,g.write_act_t,g.parent_handle,g.via
           FROM proxiima_core.goal g
           JOIN proxiima_core.goal_head h ON h.handle = g.handle
          WHERE g.owner_id = $1 AND g.schema_id = ANY($2::text[])
          ORDER BY g.t",
    )
    .bind(owner.into_inner())
    .bind(schema_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(crate::error::map_err)?;
    rows.into_iter()
        .map(|row| {
            let state = row.try_get("state").map_err(crate::error::map_err)?;
            Ok(MigrationGoal {
                handle: row.try_get("handle").map_err(crate::error::map_err)?,
                t: GoalId::new(row.try_get("t").map_err(crate::error::map_err)?),
                owner_id: OwnerId::new(row.try_get("owner_id").map_err(crate::error::map_err)?),
                schema_id: row.try_get("schema_id").map_err(crate::error::map_err)?,
                by: row.try_get("by").map_err(crate::error::map_err)?,
                via: row.try_get("via").map_err(crate::error::map_err)?,
                title: row.try_get("title").map_err(crate::error::map_err)?,
                text: row.try_get("text").map_err(crate::error::map_err)?,
                state,
                request_id: row.try_get("request_id").map_err(crate::error::map_err)?,
                close_fact_t: row.try_get("close_fact_t").map_err(crate::error::map_err)?,
                assignment_t: row.try_get("assignment_t").map_err(crate::error::map_err)?,
                dependency_t: row.try_get("dependency_t").map_err(crate::error::map_err)?,
                evidence_t: row.try_get("evidence_t").map_err(crate::error::map_err)?,
                motivated_by: row.try_get("motivated_by").map_err(crate::error::map_err)?,
                write_act_t: row.try_get("write_act_t").map_err(crate::error::map_err)?,
                parent_handle: row
                    .try_get("parent_handle")
                    .map_err(crate::error::map_err)?,
                sidecar: None,
            })
        })
        .collect()
}

async fn read_blobs(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    schema_ids: &[String],
) -> Result<Vec<MigrationBlob>, StorageError> {
    let rows = sqlx::query(
        "SELECT blob_id,owner_id,schema_id,content_hash
           FROM proxiima_core.blob
          WHERE owner_id = $1 AND schema_id = ANY($2::text[])
          ORDER BY blob_id",
    )
    .bind(owner.into_inner())
    .bind(schema_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(crate::error::map_err)?;
    rows.into_iter()
        .map(|row| {
            Ok(MigrationBlob {
                blob_id: row.try_get("blob_id").map_err(crate::error::map_err)?,
                owner_id: OwnerId::new(row.try_get("owner_id").map_err(crate::error::map_err)?),
                schema_id: row.try_get("schema_id").map_err(crate::error::map_err)?,
                content_hash: row.try_get("content_hash").map_err(crate::error::map_err)?,
            })
        })
        .collect()
}
