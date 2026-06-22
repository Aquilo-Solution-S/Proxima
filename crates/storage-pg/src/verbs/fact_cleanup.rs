//! Owner Fact-retention sweep.

use std::collections::HashSet;

use proxima_core::verbs::fact_cleanup::{CleanupDueFactsOutcome, OrphanedS3Blob};
use proxima_core::{EntityKind, Owner, OwnerPrincipalKind, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::fact_retention::get_fact_retention_in_tx;
use crate::verbs::hard_delete::{HardDeleteSet, HardDeleteSidecars, execute_hard_delete};

#[derive(Debug, sqlx::FromRow)]
struct DueFactRow {
    memory_id: Uuid,
    event_id: Vec<u8>,
    schema_id: String,
    schema_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct TombstonedDerivativeRow {
    memory_id: Uuid,
    kind: EntityKind,
    schema_id: String,
    schema_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct OrphanedS3BlobRow {
    bucket: String,
    object_key: String,
}

#[derive(Debug, Clone, Copy)]
struct OwnerColumns {
    kind: OwnerPrincipalKind,
    principal_id: Uuid,
}

#[derive(Debug, Clone, Copy)]
struct DeleteEntityEvent<'a> {
    kind: EntityKind,
    memory_id: Uuid,
    schema_id: &'a str,
    schema_version: i32,
}

/// Hard-erase due Facts and tombstone their transitive Provenance dependents.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub async fn cleanup_due_facts(
    pool: &sqlx::PgPool,
    owner: &Owner,
    fact_sidecar_tables: &[String],
    edge_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<CleanupDueFactsOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = cleanup_due_facts_in_tx(
        &mut tx,
        owner,
        fact_sidecar_tables,
        edge_sidecar_tables,
        citation_mapping_sidecar_tables,
        cited_object_sidecar_tables,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

// Pre-existing length (>100 lines) from the road-to-v1 shared-hard-delete
// refactor; this is a linear orchestration sequence, kept whole for clarity.
#[allow(clippy::too_many_lines)]
async fn cleanup_due_facts_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    fact_sidecar_tables: &[String],
    edge_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<CleanupDueFactsOutcome, StorageError> {
    let Some(retention_seconds) = get_fact_retention_in_tx(tx, owner).await? else {
        return Ok(empty_outcome());
    };

    let (owner_kind, owner_principal_id) = owner.columns();
    let owner_columns = OwnerColumns {
        kind: owner_kind,
        principal_id: owner_principal_id,
    };
    let due: Vec<DueFactRow> = sqlx::query_as(
        "SELECT memory_id, event_id, schema_id, schema_version
           FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND event_id IS NOT NULL
            AND citation_mapping_id IS NOT NULL
            AND tombstoned_at IS NULL
            AND created_at < now() - ($3::double precision * INTERVAL '1 second')
          ORDER BY created_at ASC, memory_id ASC",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(retention_seconds)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    if due.is_empty() {
        return Ok(empty_outcome());
    }

    let due_memory_ids = due.iter().map(|row| row.memory_id).collect::<Vec<_>>();
    let fact_entity_ids_to_delete = fact_entity_ids_reaching_zero(
        tx,
        owner_kind,
        owner_principal_id,
        &due_memory_ids,
    )
    .await?;
    let follow_head_edge_ids = repoint_or_delete_fact_entities(
        tx,
        owner_kind,
        owner_principal_id,
        &due_memory_ids,
    )
    .await?;
    let candidate_cited_object_ids = candidate_cited_object_ids(tx, &due_memory_ids).await?;
    let tombstoned = tombstone_transitive_derivatives(
        tx,
        owner_kind,
        owner_principal_id,
        &due_memory_ids,
    )
    .await?;
    let tombstoned_memory_ids = tombstoned
        .iter()
        .map(|row| row.memory_id)
        .collect::<Vec<_>>();
    delete_embedding_artifacts(tx, &tombstoned_memory_ids).await?;

    for row in &tombstoned {
        insert_entity_delete_event(
            tx,
            owner_columns,
            DeleteEntityEvent {
                kind: row.kind,
                memory_id: row.memory_id,
                schema_id: &row.schema_id,
                schema_version: row.schema_version,
            },
        )
        .await?;
    }
    for row in &due {
        insert_entity_delete_event(
            tx,
            owner_columns,
            DeleteEntityEvent {
                kind: EntityKind::Fact,
                memory_id: row.memory_id,
                schema_id: &row.schema_id,
                schema_version: row.schema_version,
            },
        )
        .await?;
    }

    let edge_ids = edge_ids_referencing_facts(
        tx,
        owner_kind,
        owner_principal_id,
        &due_memory_ids,
    )
    .await?;
    let edge_ids = merged_edge_ids(edge_ids, follow_head_edge_ids);
    execute_hard_delete(
        tx,
        &HardDeleteSet {
            memories: due
                .iter()
                .map(|row| (EntityKind::Fact, row.memory_id))
                .collect(),
            edge_ids,
            fact_entity_ids: fact_entity_ids_to_delete,
            event_ids: due.iter().map(|row| row.event_id.clone()).collect(),
        },
        &HardDeleteSidecars {
            memory_keyed: fact_sidecar_tables,
            edge_keyed: edge_sidecar_tables,
            citation_mapping_keyed: citation_mapping_sidecar_tables,
        },
    )
    .await?;
    let (cited_objects_erased, orphaned_s3_blobs) =
        garbage_collect_cited_objects(tx, cited_object_sidecar_tables, &candidate_cited_object_ids)
            .await?;

    Ok(CleanupDueFactsOutcome {
        facts_erased: u64::try_from(due.len()).unwrap_or(u64::MAX),
        derivatives_tombstoned: u64::try_from(tombstoned.len()).unwrap_or(u64::MAX),
        cited_objects_erased,
        orphaned_s3_blobs,
    })
}

fn empty_outcome() -> CleanupDueFactsOutcome {
    CleanupDueFactsOutcome {
        facts_erased: 0,
        derivatives_tombstoned: 0,
        cited_objects_erased: 0,
        orphaned_s3_blobs: Vec::new(),
    }
}

fn merged_edge_ids(mut edge_ids: Vec<Uuid>, follow_head_edge_ids: Vec<Uuid>) -> Vec<Uuid> {
    edge_ids.extend(follow_head_edge_ids);
    let mut seen = HashSet::with_capacity(edge_ids.len());
    edge_ids.retain(|edge_id| seen.insert(*edge_id));
    edge_ids
}

pub(crate) async fn repoint_or_delete_fact_entities(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if due_memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    repoint_fact_entity_heads(
        tx,
        owner_kind,
        owner_principal_id,
        due_memory_ids,
    )
    .await?;
    let fact_entity_ids = fact_entity_ids_reaching_zero(
        tx,
        owner_kind,
        owner_principal_id,
        due_memory_ids,
    )
    .await?;
    follow_head_edge_ids_for_entities(
        tx,
        owner_kind,
        owner_principal_id,
        &fact_entity_ids,
    )
    .await
}

async fn repoint_fact_entity_heads(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    if due_memory_ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        "WITH due_entities AS (
             SELECT DISTINCT m.fact_entity_id
               FROM proxima_core.memories m
              WHERE m.owner_principal_kind = $1
                AND m.owner_principal_id = $2
                AND m.memory_id = ANY($3::uuid[])
                AND m.fact_entity_id IS NOT NULL
         ),
         next_versions AS (
             SELECT DISTINCT ON (m.fact_entity_id)
                    m.fact_entity_id, m.memory_id, m.created_at
               FROM proxima_core.memories m
               JOIN due_entities de
                 ON de.fact_entity_id = m.fact_entity_id
              WHERE m.owner_principal_kind = $1
                AND m.owner_principal_id = $2
                AND m.memory_id <> ALL($3::uuid[])
                AND m.tombstoned_at IS NULL
              ORDER BY m.fact_entity_id, m.created_at DESC, m.memory_id DESC
         )
         UPDATE proxima_core.fact_entities fe
            SET current_memory_id = nv.memory_id,
                current_created_at = nv.created_at
           FROM next_versions nv
          WHERE fe.fact_entity_id = nv.fact_entity_id
            AND fe.owner_principal_kind = $1
            AND fe.owner_principal_id = $2
            AND fe.current_memory_id = ANY($3::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(due_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn fact_entity_ids_reaching_zero(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if due_memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar(
        "WITH due_entities AS (
             SELECT DISTINCT m.fact_entity_id
               FROM proxima_core.memories m
              WHERE m.owner_principal_kind = $1
                AND m.owner_principal_id = $2
                AND m.memory_id = ANY($3::uuid[])
                AND m.fact_entity_id IS NOT NULL
         )
         SELECT de.fact_entity_id
           FROM due_entities de
          WHERE NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.memories survivor
                     WHERE survivor.owner_principal_kind = $1
                       AND survivor.owner_principal_id = $2
                       AND survivor.fact_entity_id = de.fact_entity_id
                       AND survivor.memory_id <> ALL($3::uuid[])
                       AND survivor.tombstoned_at IS NULL
                )
          ORDER BY de.fact_entity_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn follow_head_edge_ids_for_entities(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    fact_entity_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if fact_entity_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND (
                source_fact_entity_id = ANY($3::uuid[])
                OR target_fact_entity_id = ANY($3::uuid[])
            )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(fact_entity_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn tombstone_transitive_derivatives(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<Vec<TombstonedDerivativeRow>, StorageError> {
    sqlx::query_as(
        "WITH RECURSIVE descendants(memory_id) AS (
             SELECT e.source_memory_id
               FROM proxima_core.edges e
              WHERE e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation_class = 'Provenance'
                AND e.target_memory_id = ANY($3::uuid[])
                AND e.source_memory_id IS NOT NULL
             UNION
             SELECT e.source_memory_id
               FROM descendants d
               JOIN proxima_core.edges e
                 ON e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation_class = 'Provenance'
                AND e.target_memory_id = d.memory_id
                AND e.source_memory_id IS NOT NULL
         )
         UPDATE proxima_core.memories m
            SET tombstoned_at = now()
           FROM descendants d
          WHERE m.memory_id = d.memory_id
            AND m.owner_principal_kind = $1
            AND m.owner_principal_id = $2
            AND m.kind IS NOT NULL
            AND m.tombstoned_at IS NULL
          RETURNING m.memory_id, m.kind, m.schema_id, m.schema_version",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn insert_entity_delete_event(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerColumns,
    entity: DeleteEntityEvent<'_>,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, kind,
             entity_kind, entity_memory_id, entity_schema_id, entity_schema_version)
         VALUES ($1, $2, $3, 'EntityDelete', $4, $5, $6, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(owner.kind)
    .bind(owner.principal_id)
    .bind(entity.kind)
    .bind(entity.memory_id)
    .bind(entity.schema_id)
    .bind(entity.schema_version)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn edge_ids_referencing_facts(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND (
                source_memory_id = ANY($3::uuid[])
                OR target_memory_id = ANY($3::uuid[])
                OR authorship_owner_memory_id = ANY($3::uuid[])
            )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn candidate_cited_object_ids(
    tx: &mut Transaction<'_, Postgres>,
    due_memory_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT DISTINCT cited_object_id
           FROM proxima_core.citation_mappings
          WHERE memory_id = ANY($1::uuid[])",
    )
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn orphaned_cited_object_ids(
    tx: &mut Transaction<'_, Postgres>,
    candidate_cited_object_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if candidate_cited_object_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar(
        "SELECT candidate.cited_object_id
           FROM unnest($1::uuid[]) AS candidate(cited_object_id)
          WHERE NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.citation_mappings cm
                     WHERE cm.cited_object_id = candidate.cited_object_id
                )
            AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.cited_object_uploads upload
                     WHERE upload.cited_object_id = candidate.cited_object_id
                )",
    )
    .bind(candidate_cited_object_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn orphaned_s3_blobs(
    tx: &mut Transaction<'_, Postgres>,
    orphaned_cited_object_ids: &[Uuid],
) -> Result<Vec<OrphanedS3Blob>, StorageError> {
    if orphaned_cited_object_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows: Vec<OrphanedS3BlobRow> = sqlx::query_as(
        "SELECT bucket, object_key
           FROM proxima_core.cited_uploaded_blob_v1
          WHERE cited_object_id = ANY($1::uuid[])
          ORDER BY bucket ASC, object_key ASC",
    )
    .bind(orphaned_cited_object_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|row| OrphanedS3Blob {
            bucket: row.bucket,
            object_key: row.object_key,
        })
        .collect())
}

async fn garbage_collect_cited_objects(
    tx: &mut Transaction<'_, Postgres>,
    cited_object_sidecar_tables: &[String],
    candidate_cited_object_ids: &[Uuid],
) -> Result<(u64, Vec<OrphanedS3Blob>), StorageError> {
    let orphaned_cited_object_ids =
        orphaned_cited_object_ids(tx, candidate_cited_object_ids).await?;
    let orphaned_s3_blobs = orphaned_s3_blobs(tx, &orphaned_cited_object_ids).await?;
    let cited_objects_erased =
        delete_orphaned_cited_objects(tx, cited_object_sidecar_tables, &orphaned_cited_object_ids)
            .await?;
    Ok((cited_objects_erased, orphaned_s3_blobs))
}

async fn delete_orphaned_cited_objects(
    tx: &mut Transaction<'_, Postgres>,
    cited_object_sidecar_tables: &[String],
    orphaned_cited_object_ids: &[Uuid],
) -> Result<u64, StorageError> {
    if orphaned_cited_object_ids.is_empty() {
        return Ok(0);
    }

    for table in cited_object_sidecar_tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE cited_object_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(orphaned_cited_object_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }

    let deleted = sqlx::query(
        "DELETE FROM proxima_core.cited_objects
          WHERE cited_object_id = ANY($1::uuid[])",
    )
    .bind(orphaned_cited_object_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(deleted.rows_affected())
}

async fn delete_embedding_artifacts(
    tx: &mut Transaction<'_, Postgres>,
    memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    if memory_ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        "DELETE FROM proxima_core.embeddings
          WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(())
}
